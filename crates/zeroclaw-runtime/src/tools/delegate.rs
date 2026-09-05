use crate::agent::dispatcher::{ToolDispatcher, XmlToolDispatcher};
use crate::agent::loop_::{
    LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
    TOOL_LOOP_SESSION_KEY, ToolLoop, apply_text_tool_prompt_policy, run_tool_call_loop,
};
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::approval::ApprovalManager;
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

fn invalid_semantic_completion_error(agent_name: &str) -> String {
    crate::agent::turn::outcome::semantic_empty_terminal_completion_message(Some(agent_name))
}

fn delegate_failure_error(agent_name: &str, error: &anyhow::Error) -> String {
    if error
        .chain()
        .any(|source| source.is::<zeroclaw_providers::ReliableProviderTerminalFailure>())
    {
        // Reliable's aggregate is the durable retry diagnostic for delegated
        // task records; other typed terminal failures use the delivery projection.
        return format!("Agent '{agent_name}' failed: {error}");
    }

    crate::agent::turn::outcome::terminal_completion_error_message(error, Some(agent_name))
        .unwrap_or_else(|| format!("Agent '{agent_name}' failed: {error}"))
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
    /// The daemon's shared live-config handle, when the registry that built
    /// this tool had one. `root_config` above is a *snapshot* taken at
    /// construction; every nested registry this tool builds must additionally
    /// receive this handle so the target's per-execution resolvers (plugin
    /// `[plugins.entries.config]`, `send_via` peer-group authority) follow
    /// config reloads and credential rotation instead of the startup snapshot.
    /// `None` for one-shot / non-daemon callers, which keep the documented
    /// snapshot fallback.
    live_config: Option<Arc<RwLock<Config>>>,
    /// Alias of the agent that owns this DelegateTool. Excluded from the
    /// advertised roster so an agent is never offered itself as a
    /// delegation target. Empty when unset (legacy unit-test constructors).
    caller_alias: String,
    /// The caller's live channel handles, so a `Bounded` target's channel
    /// tools can be rebuilt against its own policy without losing the route
    /// they answer on. Empty when unset (legacy unit-test constructors), in
    /// which case those tools are omitted rather than reused - an empty
    /// handle map would advertise a tool that fails at runtime.
    channel_handles: crate::tools::DelegateChannelHandles,
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

pub(crate) struct IndependentTargetTools {
    pub(crate) tools: crate::tools::scoped::ScopedToolRegistry,
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
            live_config: None,
            caller_alias: String::new(),
            channel_handles: crate::tools::DelegateChannelHandles::default(),
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
            live_config: None,
            caller_alias: String::new(),
            channel_handles: crate::tools::DelegateChannelHandles::default(),
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

    /// Attach the daemon's shared live-config handle alongside
    /// [`Self::with_root_config`].
    ///
    /// `with_root_config` supplies the snapshot this tool reads synchronously
    /// (reachability, target mode, per-target policy). This supplies the handle
    /// that every *nested* registry built for a delegated target needs so its
    /// per-execution resolvers follow reloads. Passing `None` is the documented
    /// one-shot behavior and keeps the snapshot fallback; dropping the handle
    /// when the caller has one silently pins delegated plugin tools to startup
    /// config for the parent's whole lifetime.
    pub fn with_live_config(mut self, live_config: Option<Arc<RwLock<Config>>>) -> Self {
        self.live_config = live_config;
        self
    }

    /// Supply the caller's live channel handles.
    ///
    /// `Bounded` targets get their channel tools rebuilt against their own
    /// `SecurityPolicy` while keeping these handles, so the autonomy gate
    /// becomes the target's without disconnecting delivery. Without them the
    /// affected tools are omitted: constructing them with a fresh, empty map
    /// would put a tool in the target's prompt that cannot reach any channel.
    pub fn with_channel_handles(mut self, handles: crate::tools::DelegateChannelHandles) -> Self {
        self.channel_handles = handles;
        self
    }

    /// Set the owning agent's alias so it can be excluded from the
    /// advertised delegation roster (an agent must never delegate to
    /// itself).
    pub fn with_caller_alias(mut self, alias: impl Into<String>) -> Self {
        self.caller_alias = alias.into();
        self
    }

    pub(crate) fn policy_for_target(
        &self,
        target_alias: &str,
    ) -> anyhow::Result<Arc<SecurityPolicy>> {
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
    ) -> anyhow::Result<(Box<dyn ModelProvider>, String, String)> {
        if let Some(config) = self.root_config.as_deref() {
            return crate::agent::agent::build_session_model_provider(config, model_provider, None);
        }
        let provider = zeroclaw_providers::create_model_provider_with_options(
            provider_type,
            credential,
            &self.provider_runtime_options,
        )?;
        let (_, _, model, _) = self.resolve_brain(model_provider);
        Ok((provider, provider_type.to_string(), model))
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

    /// Rebuilds a `Bounded` delegate target's shell tool with the target's
    /// OWN OS sandbox and shell timeout, instead of the `NoopSandbox`
    /// `default_tools_with_runtime` always builds (that factory is also used
    /// by several sandbox-free SOP/automation registries, so it cannot become
    /// sandbox-aware itself). Resolves sandbox/timeout the same way
    /// `all_tools_with_runtime` does for a fresh registry
    /// (`crate::tools::build_sandboxed_shell_tool`), just starting from a
    /// `SecurityPolicy` (all a `Bounded` target has) instead of a
    /// `RiskProfileConfig`.
    ///
    /// Returns `None` only when this `DelegateTool` has no `root_config`
    /// snapshot to resolve `runtime_kind`/the global shell timeout from. Per
    /// `policy_for_target` above, `root_config` being absent is the ONLY way
    /// `target_policy` can be produced without going through the
    /// root_config-requiring resolution path at all - in that case
    /// `target_policy` is literally `Arc::clone(&self.security)` (the exact
    /// same policy object as the caller's), so there is no real cross-profile
    /// privilege boundary being crossed here and the plain shell
    /// `default_tools_with_runtime` already built is safe to leave as-is.
    /// Production always sets `root_config` alongside `runtime`
    /// (`tools/mod.rs`) whenever a genuine cross-profile `target_policy` is
    /// reachable, so this `None` case is test-construction-only.
    fn rebuild_target_shell_tool(
        &self,
        target_policy: Arc<SecurityPolicy>,
        runtime: Arc<dyn crate::platform::RuntimeAdapter>,
    ) -> Option<crate::tools::ShellTool> {
        let root_config = self.root_config.as_ref()?;
        let runtime_kind = root_config.runtime.kind;
        let sandbox_cfg = target_policy.sandbox_config();
        // Mirrors the production assembly in `all_tools_with_runtime`, but
        // sourced from the TARGET's policy: the sandbox's allowed-root tiers
        // are part of the same workspace boundary this rebuild exists to
        // enforce, so inheriting the caller's here would reopen the bug, and
        // dropping them would silently narrow the target's own roots.
        let sandbox_extra_roots = crate::security::SandboxExtraRoots {
            read_write: target_policy.allowed_roots.clone(),
            read_only: target_policy.allowed_roots_read_only.clone(),
            write_only: target_policy.allowed_roots_write_only.clone(),
        };
        let sandbox = crate::security::create_sandbox(
            &sandbox_cfg,
            runtime_kind,
            Some(&target_policy.workspace_dir),
            &sandbox_extra_roots,
        );
        Some(crate::tools::build_sandboxed_shell_tool(
            target_policy,
            runtime,
            sandbox,
            root_config.shell_tool.timeout_secs,
        ))
    }

    pub(crate) async fn independent_agentic_tools_for_target(
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
            // The delegated target's registry is built once, here, but its
            // plugin tools and `send_via` authority resolve per execution. They
            // must resolve against the daemon's shared handle, not the
            // `root_config` snapshot this DelegateTool captured at
            // construction - otherwise a reload or credential rotation is
            // invisible to every delegated plugin tool for the parent's whole
            // lifetime. `None` only when the parent registry itself had no live
            // handle (one-shot callers), which keeps the snapshot fallback.
            self.live_config.clone(),
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
                acp_delivery: false,
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
            mut registry,
            activated_handle,
            ..
        } = assembled;
        // Strip the delegate tool from the ALREADY-sealed registry via the
        // `retain` mutator - no unseal/reseal round-trip through a raw `Vec`.
        // Same set removed as before (`tool.name() != Self::NAME`).
        registry.retain(|tool| tool.name() != Self::NAME);
        Ok(IndependentTargetTools {
            tools: registry,
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
        // Keep target recovery metadata local: the parent channel scope belongs to its own model call.
        let (result, fallback) = zeroclaw_providers::reliable::scope_provider_fallback(async {
            let result = self
                .execute_sync_with_admission_inner(agent_name, prompt, args, admission)
                .await;
            let fallback = zeroclaw_providers::reliable::take_last_provider_fallback_attribution();
            (result, fallback)
        })
        .await;

        let mut result = result?;
        if result.success
            && let Some(fallback) = fallback
        {
            let agentic = self
                .agents
                .get(agent_name)
                .is_some_and(|config| self.resolve_agentic(&config.runtime_profile));
            let warning =
                crate::i18n::get_required_cli_string("delegate-provider-fallback-warning");
            let header_key = if agentic {
                "delegate-provider-fallback-header-agentic"
            } else {
                "delegate-provider-fallback-header"
            };
            let header = crate::i18n::get_required_cli_string_with_args(
                header_key,
                &[
                    ("agent", agent_name),
                    ("requested_provider", &fallback.requested_candidate),
                    ("requested_model", &fallback.fallback.requested_model),
                    ("actual_provider", &fallback.actual_candidate),
                    ("actual_model", &fallback.fallback.actual_model),
                ],
            );
            // Successful delegate results are always headed by one generated line. Re-rendering
            // it here keeps the caller-visible provenance accurate without exposing rejected
            // provider diagnostics, endpoints, or credentials.
            let rendered = result
                .output
                .split_once('\n')
                .map_or(result.output.as_str(), |(_, rendered)| rendered);
            result.output = format!("{header}\n{rendered}\n\n{warning}").into();
        }
        Ok(result)
    }

    async fn execute_sync_with_admission_inner(
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
        let (legacy_provider_type, credential, _, temperature) =
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
        let (model_provider, provider_type, model) = match self.build_target_provider(
            &agent_config.model_provider,
            &legacy_provider_type,
            credential.as_deref(),
        ) {
            Ok(provider) => provider,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Failed to create model_provider '{legacy_provider_type}' for agent '{agent_name}': {e}"
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

        Ok(Self::render_non_agentic_result(
            agent_name,
            &provider_type,
            &model,
            result,
        ))
    }

    fn render_non_agentic_result(
        agent_name: &str,
        provider_type: &str,
        model: &str,
        result: anyhow::Result<String>,
    ) -> ToolResult {
        match result {
            Ok(response)
                if zeroclaw_api::model_provider::strip_think_tags(&response).is_empty() =>
            {
                ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(invalid_semantic_completion_error(agent_name)),
                }
            }
            Ok(response) => ToolResult {
                success: true,
                output: format!("[Agent '{agent_name}' ({provider_type}/{model})]\n{response}",)
                    .into(),
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(delegate_failure_error(agent_name, &e)),
            },
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
        // Carried, not dropped: the background task rebuilds a DelegateTool that
        // will construct its own nested registries.
        let live_config = self.live_config.clone();
        let caller_alias = self.caller_alias.clone();
        let channel_handles = self.channel_handles.clone();
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
                    live_config,
                    caller_alias,
                    channel_handles,
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
            // Carried, not dropped: each fan-out task rebuilds a DelegateTool
            // that will construct its own nested registries.
            let live_config = self.live_config.clone();
            let caller_alias = self.caller_alias.clone();
            let channel_handles = self.channel_handles.clone();
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
                        live_config,
                        caller_alias,
                        channel_handles,
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
        let mut resolved_agent_config = agent_config.clone();
        resolved_agent_config.resolved = self.resolve_loop_runtime(agent_alias, agent_config);
        let agent_config = &resolved_agent_config;

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

        let empty_tools: &[Box<dyn Tool>] = &[];
        let expose_text_tools =
            sends_native_tool_specs || !agent_config.resolved.strict_tool_parsing;
        let prompt_tools = if expose_text_tools {
            sub_tools
        } else {
            empty_tools
        };

        let shell_profile = self.runtime.as_ref().and_then(|r| r.shell_profile());

        // Build structured operational context using SystemPromptBuilder sections.
        let dispatcher_instructions = if sends_native_tool_specs || prompt_tools.is_empty() {
            String::new()
        } else {
            XmlToolDispatcher.prompt_instructions(prompt_tools)
        };
        let ctx = PromptContext {
            workspace_dir,
            agent_workspace_dir: workspace_dir,
            model_name,
            tools: prompt_tools,
            skills,
            skills_prompt_mode: agent_config.resolved.prompt_injection_mode,
            identity_config: None,
            interaction: None,
            dispatcher_instructions: &dispatcher_instructions,
            sends_native_tool_specs: sends_native_tool_specs && !prompt_tools.is_empty(),
            security_summary: None,
            autonomy_level: crate::security::AutonomyLevel::default(),
            shell_profile,
        };

        let builder = SystemPromptBuilder::default()
            .add_section(Box::new(crate::agent::prompt::ToolsSection))
            .add_section(Box::new(crate::agent::prompt::SafetySection))
            .add_section(Box::new(crate::agent::prompt::ShellSection))
            .add_section(Box::new(crate::agent::prompt::SkillsSection))
            .add_section(Box::new(crate::agent::prompt::WorkspaceSection))
            .add_section(Box::new(crate::agent::prompt::RuntimeSection))
            .add_section(Box::new(crate::agent::prompt::DateTimeSection));

        let mut enriched = builder.build(&ctx).unwrap_or_default();

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
        // Independent delegates are fresh, non-interactive target turns. Give the
        // nested loop a fresh manager from the target profile so prompt-required
        // tools fail closed before dispatch; built-in shell remains ungated here
        // and receives approved=false for its own command-policy enforcement.
        let approval_manager = if target_mode == DelegateExecutionMode::Independent {
            self.root_config
                .as_ref()
                .and_then(|config| config.risk_profile_for_agent(agent_name))
                .map(ApprovalManager::for_non_interactive)
        } else {
            None
        };
        // Deferred-MCP side-channels for an INDEPENDENT target: its sub-agent turn must
        // inject the deferred-tools prompt section and thread the activated set, exactly as
        // a fresh target turn does. Bounded delegation leaves these empty (it starts from
        // the parent's already-built registry, not the target's assembled one).
        let mut sub_deferred_section = String::new();
        let mut sub_activated: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>> = None;
        // Build the sub-agent's system prompt (skills, identity) from the TARGET's
        // workspace, not the caller's - so skill *prompt* content matches the skill
        // *tools* assembled above. Populated by BOTH modes now; `None` only survives
        // when there is no `root_config` to resolve the target's workspace from, which
        // is the config-less unit-test path.
        let mut sub_workspace: Option<PathBuf> = None;
        // The target's canonical skills, so the prompt's SkillsSection describes exactly
        // the assembled skill tools rather than the local bundle resolver's narrower
        // view - and, for bounded, rather than the caller's own skills.
        let mut sub_skills: Option<Vec<crate::skills::Skill>> = None;
        let sub_tools: crate::tools::scoped::ScopedToolRegistry = match target_mode {
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
                // Published once the target's registry is sealed, and read by the
                // rebuilt `spawn_subagent` when it runs. It cannot be a value
                // here: the set it must carry is the OUTCOME of the filtering
                // below, and the tools that will consult it have to exist
                // before that filtering can substitute them in.
                let bounded_ceiling: Arc<std::sync::OnceLock<Vec<String>>> =
                    Arc::new(std::sync::OnceLock::new());
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
                        Ok(Some(memory)) => {
                            Self::memory_tools_for_target(memory, Arc::clone(&target_policy))
                                .into_iter()
                                .map(|tool| (tool.name().to_string(), tool))
                                .collect()
                        }
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

                // Filesystem-boundary tools (shell/file_read/file_write/...) must be
                // rebuilt against the TARGET's own SecurityPolicy, never reused from
                // `self.parent_tools`: those instances were built once for the CALLER
                // and bake its `workspace_dir`/`allowed_roots`/`forbidden_paths` into a
                // private field, so a Bounded target with a different risk profile would
                // otherwise silently act inside the caller's workspace. Built via the
                // SAME canonical factory production uses for a fresh registry
                // (`default_tools_with_runtime` + `image_info_tool`), so this can't
                // drift from the real tool-construction path over time - mirrors the
                // memory rebuild above.
                let needs_fs_tools = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools.iter().any(|tool| {
                        self.security.is_tool_allowed(tool.name())
                            && crate::tools::FILESYSTEM_TOOL_NAMES.contains(&tool.name())
                            && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
                    })
                };
                // No runtime adapter means the target's OWN filesystem tools cannot
                // be built. That is not a reason to abort the whole delegation: the
                // assembly seam below drops some filesystem-boundary tools anyway
                // (`deliver_file` without an ACP transport, for one), so a hard error
                // here would fail delegations that were always going to end up
                // without the tool. Instead the unbuildable names are OMITTED from
                // the target registry further down - fail-closed, because handing
                // over the caller's instance is precisely the boundary bug this
                // rebuild exists to prevent.
                let fs_runtime = if needs_fs_tools {
                    self.runtime.as_ref().cloned()
                } else {
                    None
                };
                let filesystem_tools_unavailable = needs_fs_tools && fs_runtime.is_none();
                let mut target_fs_tools: HashMap<String, Box<dyn Tool>> =
                    if let Some(runtime) = fs_runtime {
                        let mut tools = crate::tools::default_tools_with_runtime(
                            Arc::clone(&target_policy),
                            runtime.clone(),
                        );
                        tools.push(crate::tools::image_info_tool(Arc::clone(&target_policy)));
                        let mut tools: HashMap<String, Box<dyn Tool>> = tools
                            .into_iter()
                            .map(|tool| (tool.name().to_string(), tool))
                            .collect();
                        // The shell entry `default_tools_with_runtime` just built carries
                        // `NoopSandbox` unconditionally (that factory also backs several
                        // sandbox-free SOP/automation registries, so it can't become
                        // sandbox-aware itself) - swap it for one rebuilt with the target's
                        // own configured OS sandbox and shell-timeout contract.
                        if let Some(shell) =
                            self.rebuild_target_shell_tool(Arc::clone(&target_policy), runtime)
                        {
                            tools.insert(
                                "shell".to_string(),
                                Box::new(crate::tools::RateLimitedTool::new(
                                    shell,
                                    Arc::clone(&target_policy),
                                )) as Box<dyn Tool>,
                            );
                        }
                        tools
                    } else {
                        HashMap::new()
                    };

                // Tools that bind `workspace_dir`/`SecurityPolicy` state at construction
                // time the exact same way as the filesystem tools above, but are only
                // built by `all_tools_with_runtime` (config-gated, several disabled by
                // default) - see `WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT`. Rebuilt via
                // the SAME per-tool factories `all_tools_with_runtime` calls
                // (`crate::tools::git_operations_tool`, `backup_tool`, ...), so this can't
                // drift from the real construction path either.
                let needs_workspace_bound_tools = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools.iter().any(|tool| {
                        self.security.is_tool_allowed(tool.name())
                            && crate::tools::WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT
                                .contains(&tool.name())
                            && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
                    })
                };
                // Tools that bind the CALLER's `agent_alias` (not `workspace_dir`/
                // `SecurityPolicy`) at construction time - see `IDENTITY_BOUND_TOOL_NAMES`.
                // Same underlying bug, different capture mechanism: reused unchanged, these
                // act using the caller's identity instead of the target's.
                let needs_identity_bound_tools = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools.iter().any(|tool| {
                        self.security.is_tool_allowed(tool.name())
                            && crate::tools::IDENTITY_BOUND_TOOL_NAMES.contains(&tool.name())
                            && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
                    })
                };
                let mut target_workspace_bound_tools: HashMap<String, Box<dyn Tool>> =
                    HashMap::new();
                let mut target_identity_bound_tools: HashMap<String, Box<dyn Tool>> =
                    HashMap::new();
                if (needs_workspace_bound_tools || needs_identity_bound_tools)
                    && let Some(root_config) = self.root_config.as_ref()
                {
                    target_workspace_bound_tools.insert(
                        "git_operations".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::git_operations_tool(
                            Arc::clone(&target_policy),
                            &target_policy.workspace_dir,
                        ))) as Box<dyn Tool>,
                    );
                    if let Some(tool) =
                        crate::tools::backup_tool(&target_policy.workspace_dir, root_config)
                    {
                        target_workspace_bound_tools
                            .insert("backup".to_string(), Box::new(ToolArcRef::new(tool)));
                    }
                    if let Some(tool) = crate::tools::data_management_tool(
                        &target_policy.workspace_dir,
                        root_config,
                    ) {
                        target_workspace_bound_tools.insert(
                            "data_management".to_string(),
                            Box::new(ToolArcRef::new(tool)),
                        );
                    }
                    if let Some(tool) = crate::tools::linkedin_tool(
                        Arc::clone(&target_policy),
                        &target_policy.workspace_dir,
                        root_config,
                    ) {
                        target_workspace_bound_tools
                            .insert("linkedin".to_string(), Box::new(ToolArcRef::new(tool)));
                    }
                    if let Some(tool) = crate::tools::claude_code_runner_tool(
                        Arc::clone(&target_policy),
                        root_config,
                    ) {
                        target_workspace_bound_tools.insert(
                            "claude_code_runner".to_string(),
                            Box::new(ToolArcRef::new(tool)),
                        );
                    }
                    target_workspace_bound_tools.insert(
                        "pushover".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::pushover_tool(
                            Arc::clone(&target_policy),
                            &target_policy.workspace_dir,
                        ))),
                    );
                    target_workspace_bound_tools.insert(
                        "screenshot".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::screenshot_tool(Arc::clone(
                            &target_policy,
                        )))),
                    );
                    if let Some(tool) =
                        crate::tools::file_upload_tool(Arc::clone(&target_policy), root_config)
                    {
                        target_workspace_bound_tools
                            .insert("file_upload".to_string(), Box::new(ToolArcRef::new(tool)));
                    }
                    if let Some(tool) = crate::tools::file_upload_bundle_tool(
                        Arc::clone(&target_policy),
                        root_config,
                    ) {
                        target_workspace_bound_tools.insert(
                            "file_upload_bundle".to_string(),
                            Box::new(ToolArcRef::new(tool)),
                        );
                    }
                    if let Some(tool) =
                        crate::tools::browser_tool(Arc::clone(&target_policy), &root_config.browser)
                    {
                        target_workspace_bound_tools
                            .insert("browser".to_string(), Box::new(ToolArcRef::new(tool)));
                    }

                    // Tools bound to the CALLER's `agent_alias` (not
                    // `workspace_dir`/`SecurityPolicy`) at construction time -
                    // see `IDENTITY_BOUND_TOOL_NAMES`. Rebuilt via the SAME
                    // per-tool factories `all_tools_with_runtime` calls, against
                    // the target's own `agent_name`, not the caller's alias.
                    if let Some(tool) =
                        crate::tools::read_skill_tool(Arc::clone(root_config), agent_name)
                    {
                        target_identity_bound_tools
                            .insert("read_skill".to_string(), Box::new(ToolArcRef::new(tool)));
                    }
                    target_identity_bound_tools.insert(
                        "cron_remove".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::cron_remove_tool(
                            Arc::clone(root_config),
                            Arc::clone(&target_policy),
                            agent_name,
                        ))),
                    );
                    target_identity_bound_tools.insert(
                        "cron_list".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::cron_list_tool(
                            Arc::clone(root_config),
                            agent_name,
                        ))),
                    );
                    target_identity_bound_tools.insert(
                        "cron_runs".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::cron_runs_tool(
                            Arc::clone(root_config),
                            agent_name,
                        ))),
                    );
                    if let Some(tool) = crate::tools::llm_task_tool(
                        Arc::clone(&target_policy),
                        root_config,
                        agent_name,
                    ) {
                        target_identity_bound_tools
                            .insert("llm_task".to_string(), Box::new(ToolArcRef::new(tool)));
                    }
                    // Approval authority is the caller's, and it cannot be
                    // rebuilt for the target: the target may simply not be in
                    // the checkpoint's required group. A refusing stub with the
                    // real schema is clearer to the model than a missing tool.
                    target_identity_bound_tools.insert(
                        "sop_approve".to_string(),
                        Box::new(crate::tools::sop_approve::BoundedSopApproveDenied)
                            as Box<dyn Tool>,
                    );
                    target_identity_bound_tools.insert(
                        "send_message_to_peer".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::send_message_to_peer_tool(
                            Arc::clone(root_config),
                            agent_name,
                        ))),
                    );
                    target_identity_bound_tools.insert(
                        "spawn_subagent".to_string(),
                        Box::new(ToolArcRef::new(crate::tools::spawn_subagent_tool(
                            Arc::clone(root_config),
                            agent_name,
                            Arc::clone(&target_policy),
                            false,
                            Some(Arc::clone(&bounded_ceiling)),
                        ))),
                    );

                    if let Some(runtime) = self.runtime.as_ref() {
                        let persistent_writes = runtime.has_filesystem_access();
                        if let Some(tool) = crate::tools::file_download_tool(
                            Arc::clone(&target_policy),
                            root_config,
                            persistent_writes,
                        ) {
                            target_workspace_bound_tools.insert(
                                "file_download".to_string(),
                                Box::new(ToolArcRef::new(tool)),
                            );
                        }
                        if let Some(tool) = crate::tools::image_gen_tool(
                            Arc::clone(&target_policy),
                            &target_policy.workspace_dir,
                            root_config,
                            persistent_writes,
                        ) {
                            target_workspace_bound_tools
                                .insert("image_gen".to_string(), Box::new(ToolArcRef::new(tool)));
                        }
                        target_identity_bound_tools.insert(
                            "cron_add".to_string(),
                            Box::new(ToolArcRef::new(crate::tools::cron_add_tool(
                                Arc::clone(root_config),
                                Arc::clone(&target_policy),
                                agent_name,
                                Arc::clone(runtime),
                            ))),
                        );
                        target_identity_bound_tools.insert(
                            "cron_run".to_string(),
                            Box::new(ToolArcRef::new(crate::tools::cron_run_tool(
                                Arc::clone(root_config),
                                Arc::clone(&target_policy),
                                agent_name,
                                Arc::clone(runtime),
                            ))),
                        );
                        target_identity_bound_tools.insert(
                            "cron_update".to_string(),
                            Box::new(ToolArcRef::new(crate::tools::cron_update_tool(
                                Arc::clone(root_config),
                                Arc::clone(&target_policy),
                                agent_name,
                                Arc::clone(runtime),
                            ))),
                        );
                        target_identity_bound_tools.insert(
                            "schedule".to_string(),
                            Box::new(ToolArcRef::new(crate::tools::schedule_tool(
                                Arc::clone(&target_policy),
                                root_config.as_ref().clone(),
                                agent_name,
                                Arc::clone(runtime),
                            ))),
                        );

                        let register_coding_cli_tools =
                            runtime.has_shell_access() && persistent_writes;
                        let needs_shared_coding_cli_executor = {
                            let parent_tools = self.parent_tools.read();
                            ["claude_code", "codex_cli", "gemini_cli", "opencode_cli"]
                                .iter()
                                .any(|name| {
                                    parent_tools.iter().any(|tool| {
                                        tool.name() == *name
                                            && self.security.is_tool_allowed(tool.name())
                                            && Self::delegate_admits_with_mcp(
                                                &tool_policy,
                                                tool.name(),
                                            )
                                    })
                                })
                        };
                        if needs_shared_coding_cli_executor {
                            // Same sandbox-resolution recipe as `rebuild_target_shell_tool`.
                            // Resolved independently here rather than sharing that call's
                            // result: both are cheap, pure, deterministic computations from
                            // the same `target_policy`, so a second call cannot diverge from
                            // the first - reusing one object would only save a little work,
                            // not add any correctness guarantee.
                            let runtime_kind = root_config.runtime.kind;
                            let sandbox_cfg = target_policy.sandbox_config();
                            // Mirrors the production assembly in `all_tools_with_runtime`, but
                            // sourced from the TARGET's policy: the sandbox's allowed-root tiers
                            // are part of the same workspace boundary this rebuild exists to
                            // enforce, so inheriting the caller's here would reopen the bug, and
                            // dropping them would silently narrow the target's own roots.
                            let sandbox_extra_roots = crate::security::SandboxExtraRoots {
                                read_write: target_policy.allowed_roots.clone(),
                                read_only: target_policy.allowed_roots_read_only.clone(),
                                write_only: target_policy.allowed_roots_write_only.clone(),
                            };
                            let sandbox = crate::security::create_sandbox(
                                &sandbox_cfg,
                                runtime_kind,
                                Some(&target_policy.workspace_dir),
                                &sandbox_extra_roots,
                            );
                            let executor =
                                crate::tools::coding_cli_executor::RuntimeCodingCliExecutor::shared(
                                    Arc::clone(runtime),
                                    sandbox,
                                    root_config.runtime.kind
                                        == zeroclaw_config::schema::RuntimeKind::Native,
                                );

                            if let Some(tool) = crate::tools::claude_code_tool(
                                Arc::clone(&target_policy),
                                root_config,
                                register_coding_cli_tools,
                                &executor,
                            ) {
                                target_workspace_bound_tools.insert(
                                    "claude_code".to_string(),
                                    Box::new(ToolArcRef::new(tool)),
                                );
                            }
                            if let Some(tool) = crate::tools::codex_cli_tool(
                                Arc::clone(&target_policy),
                                root_config,
                                register_coding_cli_tools,
                                &executor,
                            ) {
                                target_workspace_bound_tools.insert(
                                    "codex_cli".to_string(),
                                    Box::new(ToolArcRef::new(tool)),
                                );
                            }
                            if let Some(tool) = crate::tools::gemini_cli_tool(
                                Arc::clone(&target_policy),
                                root_config,
                                register_coding_cli_tools,
                                &executor,
                            ) {
                                target_workspace_bound_tools.insert(
                                    "gemini_cli".to_string(),
                                    Box::new(ToolArcRef::new(tool)),
                                );
                            }
                            if let Some(tool) = crate::tools::opencode_cli_tool(
                                Arc::clone(&target_policy),
                                root_config,
                                register_coding_cli_tools,
                                &executor,
                            ) {
                                target_workspace_bound_tools.insert(
                                    "opencode_cli".to_string(),
                                    Box::new(ToolArcRef::new(tool)),
                                );
                            }
                        }
                    }
                }

                // Channel tools: the caller's policy AND a live channel handle
                // are both captured at construction. Rebuilt against
                // `target_policy` while KEEPING the caller's handle, so the
                // autonomy gate becomes the target's and the delivery route
                // stays connected (see `CHANNEL_REBOUND_TOOL_NAMES`).
                //
                // A handle this DelegateTool never received means the tool is
                // OMITTED: building it with a fresh, empty map would advertise
                // a tool to the target that cannot reach any channel.
                let needs_channel_tools = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools.iter().any(|tool| {
                        self.security.is_tool_allowed(tool.name())
                            && (crate::tools::CHANNEL_REBOUND_TOOL_NAMES.contains(&tool.name())
                                || tool.name() == "send_via")
                            && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
                    })
                };
                let mut target_channel_tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
                if needs_channel_tools && let Some(root_config) = self.root_config.as_ref() {
                    let handles = &self.channel_handles;
                    let mut insert = |name: &str, tool: Arc<dyn Tool>| {
                        target_channel_tools.insert(
                            name.to_string(),
                            Box::new(ToolArcRef::new(tool)) as Box<dyn Tool>,
                        );
                    };
                    if let Some(handle) = handles.ask_user.as_ref() {
                        insert(
                            "ask_user",
                            crate::tools::ask_user_tool(
                                Arc::clone(&target_policy),
                                Arc::clone(handle),
                            ),
                        );
                        // `send_via` shares `ask_user`'s handle in production.
                        // Its alias capture is a closure over the peer-group
                        // resolver, not a field, so it must be rebuilt with the
                        // TARGET's alias or the target routes through the
                        // caller's peer groups.
                        insert(
                            "send_via",
                            crate::tools::send_via_tool(
                                Arc::clone(&target_policy),
                                root_config,
                                self.live_config.clone(),
                                agent_name,
                                Arc::clone(handle),
                            ),
                        );
                    }
                    if let Some(handle) = handles.poll.as_ref() {
                        insert(
                            "poll",
                            crate::tools::poll_tool(Arc::clone(&target_policy), Arc::clone(handle)),
                        );
                    }
                    if let Some(handle) = handles.reaction.as_ref() {
                        insert(
                            "reaction",
                            crate::tools::reaction_tool(
                                Arc::clone(&target_policy),
                                Arc::clone(handle),
                            ),
                        );
                        // `git_forge` shares the `reaction` handle in production.
                        insert(
                            "git_forge",
                            crate::tools::git_forge_tool(
                                Arc::clone(&target_policy),
                                Arc::clone(handle),
                            ),
                        );
                    }
                    if let Some(handle) = handles.channel_room.as_ref() {
                        insert(
                            "channel_room",
                            crate::tools::channel_room_tool(
                                Arc::clone(&target_policy),
                                Arc::clone(handle),
                            ),
                        );
                    }
                    if let Some(handle) = handles.escalate.as_ref() {
                        insert(
                            "escalate_to_human",
                            crate::tools::escalate_to_human_tool(
                                Arc::clone(&target_policy),
                                root_config.escalation.alert_channels.clone(),
                                Arc::clone(handle),
                            ),
                        );
                    }
                }

                // Tools that capture the caller's `SecurityPolicy` only for its
                // autonomy/rate gate, with global credentials and endpoint
                // config - see `AUTONOMY_REBOUND_TOOL_NAMES`. Reused unchanged
                // they let a read-only target act under the CALLER's autonomy,
                // so they are rebuilt from `target_policy` through the SAME
                // factories `all_tools_with_runtime` uses, which keeps their
                // config gates and wrapper stacks from drifting apart.
                let needs_autonomy_tools = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools.iter().any(|tool| {
                        self.security.is_tool_allowed(tool.name())
                            && crate::tools::AUTONOMY_REBOUND_TOOL_NAMES.contains(&tool.name())
                            && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
                    })
                };
                let mut target_autonomy_tools: HashMap<String, Box<dyn Tool>> = HashMap::new();
                if needs_autonomy_tools && let Some(root_config) = self.root_config.as_ref() {
                    let mut insert = |tool: Option<Arc<dyn Tool>>| {
                        if let Some(tool) = tool {
                            target_autonomy_tools.insert(
                                tool.name().to_string(),
                                Box::new(ToolArcRef::new(tool)) as Box<dyn Tool>,
                            );
                        }
                    };
                    // Shell-gated registrations must see the same answer the
                    // production path would: no runtime adapter means no shell.
                    let has_shell_access = self
                        .runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.has_shell_access());
                    insert(crate::tools::http_request_tool(
                        Arc::clone(&target_policy),
                        &root_config.http_request,
                        root_config,
                    ));
                    insert(crate::tools::web_fetch_tool(
                        Arc::clone(&target_policy),
                        &root_config.web_fetch,
                        root_config,
                    ));
                    insert(crate::tools::text_browser_tool(
                        Arc::clone(&target_policy),
                        root_config,
                    ));
                    insert(crate::tools::browser_open_tool(
                        Arc::clone(&target_policy),
                        &root_config.browser,
                    ));
                    insert(crate::tools::browser_delegate_tool(
                        Arc::clone(&target_policy),
                        root_config,
                        has_shell_access,
                    ));
                    insert(crate::tools::notion_tool(
                        Arc::clone(&target_policy),
                        root_config,
                    ));
                    insert(crate::tools::web_search_tool(
                        Arc::clone(&target_policy),
                        root_config,
                    ));
                    insert(crate::tools::jira_tool(
                        Arc::clone(&target_policy),
                        root_config,
                    ));
                    insert(crate::tools::google_workspace_tool(
                        Arc::clone(&target_policy),
                        root_config,
                        has_shell_access,
                    ));
                    // Composio's key and entity id are the global config values,
                    // resolved exactly as the independent path resolves them.
                    let (composio_key, composio_entity_id) = if root_config.composio.enabled {
                        (
                            root_config.composio.api_key.as_deref(),
                            Some(root_config.composio.entity_id.as_str()),
                        )
                    } else {
                        (None, None)
                    };
                    insert(crate::tools::composio_tool(
                        Arc::clone(&target_policy),
                        composio_key,
                        composio_entity_id,
                    ));
                    // A misconfigured `microsoft365` aborts the production
                    // registry; here the delegate simply omits it, which is the
                    // fail-closed reading of the same signal.
                    if let crate::tools::Microsoft365Registration::Tool(tool) =
                        crate::tools::microsoft365_tool(
                            Arc::clone(&target_policy),
                            root_config,
                            &target_policy.workspace_dir,
                        )
                    {
                        insert(Some(tool));
                    }
                    insert(Some(crate::tools::model_routing_config_tool(
                        Arc::clone(&target_policy),
                        Arc::clone(root_config),
                    )));
                    insert(Some(crate::tools::proxy_config_tool(
                        Arc::clone(&target_policy),
                        Arc::clone(root_config),
                    )));
                    if let Ok(backend) = zeroclaw_infra::make_session_backend(
                        &root_config.data_dir,
                        &root_config.channels.session_backend,
                    ) {
                        insert(Some(crate::tools::sessions_history_tool(
                            Arc::clone(&target_policy),
                            backend.clone(),
                        )));
                        insert(Some(crate::tools::sessions_send_tool(
                            Arc::clone(&target_policy),
                            backend,
                        )));
                    }
                }

                // Deny by default. A bounded target always crosses at least the
                // alias boundary - neither reachability path lets an agent
                // delegate to itself - so reuse of a caller-built instance has to
                // be EARNED by name instead of being the fallback. Anything that
                // was not rebuilt against the target's own context above and is
                // not proven free of caller capture is omitted from the target
                // registry rather than handed over.
                //
                // `root_config` is what makes a target context resolvable at all:
                // without it nothing above was rebuilt and `target_policy` IS the
                // caller's policy, so that path keeps its current behaviour.
                let deny_unclassified_reuse = self.root_config.is_some();
                // D3: an MCP tool belongs to the target when the TARGET's own
                // `mcp_bundles` grant the server it is prefixed with. Matched
                // against the resolved server list rather than by splitting the
                // name on `__`, because nothing stops a server name from
                // containing `__` itself.
                let target_mcp_servers: Vec<String> = self
                    .root_config
                    .as_deref()
                    .map(|config| {
                        config
                            .mcp_servers_for_agent(agent_name)
                            .into_iter()
                            .map(|server| server.name)
                            .collect()
                    })
                    .unwrap_or_default();

                // Build the bounded tool set: the parent's tools, filtered by the
                // caller's own `is_tool_allowed` + `delegate_admits_with_mcp`, with
                // the target's own rebuilt memory / filesystem / workspace-bound /
                // identity-bound instances substituted in. The `parent_tools` read
                // guard is scoped to this block so it drops BEFORE the
                // `assemble().await` below - a parking_lot guard held across an await
                // would make the delegate future `!Send`.
                let filtered: Vec<Box<dyn Tool>> = {
                    let parent_tools = self.parent_tools.read();
                    parent_tools
                        .iter()
                        .filter(|tool| tool.name() != Self::NAME)
                        .filter(|tool| self.security.is_tool_allowed(tool.name()))
                        .filter(|tool| Self::delegate_admits_with_mcp(&tool_policy, tool.name()))
                        .filter_map(|tool| {
                            if filesystem_tools_unavailable
                                && crate::tools::FILESYSTEM_TOOL_NAMES.contains(&tool.name())
                            {
                                // The target's own instance could not be built, so the
                                // caller's must not be substituted for it.
                                return None;
                            }
                            if let Some(rebuilt) = target_memory_tools
                                .remove(tool.name())
                                .or_else(|| target_fs_tools.remove(tool.name()))
                                .or_else(|| target_workspace_bound_tools.remove(tool.name()))
                                .or_else(|| target_identity_bound_tools.remove(tool.name()))
                                .or_else(|| target_autonomy_tools.remove(tool.name()))
                                .or_else(|| target_channel_tools.remove(tool.name()))
                            {
                                return Some(rebuilt);
                            }
                            // MCP tools are decided by SERVER IDENTITY, asked of
                            // the registry the instance itself carries, and are
                            // rebuilt against the target's policy. Matching the
                            // name against `<granted>__` admitted any server whose
                            // name merely started with a granted one, and reusing
                            // the instance kept the caller's workspace as the
                            // destination for materialized attachments.
                            //
                            // An MCP instance whose server cannot be resolved is
                            // omitted: an unroutable name is not a grant.
                            if let Some(wrapper) = tool
                                .as_any()
                                .and_then(|any| any.downcast_ref::<crate::tools::McpToolWrapper>())
                            {
                                let granted = wrapper.server_name().is_some_and(|server| {
                                    target_mcp_servers.iter().any(|name| name == server)
                                });
                                return if granted {
                                    Some(Box::new(wrapper.rebound(Arc::clone(&target_policy)))
                                        as Box<dyn Tool>)
                                } else {
                                    None
                                };
                            }
                            let reuse_is_earned = !deny_unclassified_reuse
                                || crate::tools::SAFE_FOR_BOUNDED_REUSE.contains(&tool.name());
                            if reuse_is_earned {
                                Some(Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>)
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                // Seal the already-filtered set through the one assembly seam.
                // The policy is `SecurityPolicy::default()` (no allow/deny
                // lists), so `assemble`'s built-in filter is a provable identity
                // over `filtered`: it drops nothing the delegate filter kept.
                // Re-applying `self.security` here would double-filter and could
                // REGRESS delegate scoping, so it is deliberately NOT reused. No
                // peripherals / MCP / skills / memory-strip. A default config is
                // load-bearing here: the caller's config could synthesize pipeline
                // tools and violate the bounded parent-registry ceiling.
                let bounded_default_config = Config::default();
                let bounded_security = Arc::new(SecurityPolicy::default());
                let assembled_bounded = crate::tools::scoped::ScopedToolRegistry::assemble(
                    crate::tools::scoped::ScopedAssembly {
                        config: &bounded_default_config,
                        agent_alias: agent_name,
                        security: &bounded_security,
                        built: crate::tools::AllToolsResult::from_prebuilt_tools(filtered),
                        // Empty is load-bearing: bounded children inherit no target skill
                        // tools, and a non-empty list would make this default policy active.
                        skills: &[],
                        runtime: Arc::new(crate::platform::NativeRuntime::new()),
                        caller_allowed: None,
                        connect_mcp: false,
                        connect_peripherals: false,
                        exclude_memory: false,
                        acp_delivery: false,
                        list_deferred_mcp_specs: false,
                        emit_assembly_logs: false,
                        mcp_registry: None,
                    },
                )
                .await;
                // The sealed set IS the ceiling for everything below this hop:
                // bounding each level by what the level above actually received
                // makes the bound narrow monotonically, instead of restating the
                // original caller's set at every depth and drifting from it.
                let _ = bounded_ceiling.set(
                    assembled_bounded
                        .registry
                        .iter()
                        .map(|tool| tool.name().to_string())
                        .collect(),
                );
                // The prompt must describe the TARGET's skills and workspace, not
                // the caller's. Every skill-bearing tool above is already the
                // target's, so building the prompt from `self.workspace_dir`
                // would describe skills the target has no tools for and omit the
                // ones it does - the tools-from-B / prompt-from-A split the
                // independent path already avoids.
                //
                // Populating these two covers BOTH skill-resolution branches at
                // once: `sub_skills` short-circuits the resolver entirely (so
                // neither the default `skills_dir` branch nor the
                // `skill_bundles` branch can join a directory onto the caller's
                // workspace), and `sub_workspace` is what reaches
                // `PromptContext`. Same source the independent path uses.
                if let Some(root_config) = self.root_config.as_ref() {
                    // The EXECUTION policy's workspace, not the target's
                    // configured one: a same-profile hand-off deliberately keeps
                    // the caller's session workspace on that policy, and the
                    // file tools resolve relative paths against it. Naming the
                    // configured directory here would describe a directory the
                    // model's own writes never reach. Skill resolution below
                    // stays keyed to the target's identity, which is a separate
                    // question from where the work lands.
                    sub_workspace = Some(target_policy.workspace_dir.clone());
                    sub_skills = Some(crate::skills::load_skills_for_agent_from_config(
                        root_config,
                        agent_name,
                    ));
                }
                assembled_bounded.registry
            }
        };

        let loop_runtime = self.resolve_loop_runtime(agent_name, agent_config);
        let native_tools = model_provider
            .capabilities_for_model(model)
            .native_tool_calling;

        // Independent delegates execute as target-owned turns, so their thinking policy
        // must override the parent task-local scope for the child loop. Bounded delegates
        // deliberately retain the caller's turn context.
        let thinking_params = (target_mode == DelegateExecutionMode::Independent).then(|| {
            crate::agent::thinking::apply_thinking_level_with_config(
                loop_runtime.thinking.default_level,
                &loop_runtime.thinking,
            )
        });
        let effective_temperature = thinking_params.as_ref().map_or(temperature, |params| {
            temperature.map(|value| {
                crate::agent::thinking::clamp_temperature(value + params.temperature_adjustment)
            })
        });

        // Build enriched system prompt with tools, skills, workspace, datetime context.
        // Both modes build it from the TARGET's workspace (`sub_workspace`), so the skill
        // prompt content matches the target's skill tools. The fallback to
        // `self.workspace_dir` is the config-less path, where there is no target workspace
        // to resolve and `target_policy` is the caller's policy anyway.
        let prompt_workspace = sub_workspace.as_deref().unwrap_or(&self.workspace_dir);
        let enriched_system_prompt = self.build_enriched_system_prompt(
            agent_name,
            agent_config,
            model,
            &sub_tools,
            prompt_workspace,
            native_tools,
            sub_skills.as_deref(),
        );
        // Independent delegates surface the target's deferred MCP tools the way a fresh
        // target turn does. See `compose_independent_system_prompt`: it applies the turn
        // engine's text-tool prompt policy to the deferred section (so a non-native strict
        // target suppresses it, exactly as a fresh turn would) and then appends it.
        let enriched_system_prompt = Self::compose_independent_system_prompt(
            enriched_system_prompt,
            sub_deferred_section,
            native_tools,
            loop_runtime.strict_tool_parsing,
        );
        let enriched_system_prompt = match (
            enriched_system_prompt,
            thinking_params
                .as_ref()
                .and_then(|params| params.system_prompt_prefix.as_deref()),
        ) {
            (Some(prompt), Some(prefix)) => Some(format!("{prefix}\n\n{prompt}")),
            (None, Some(prefix)) => Some(prefix.to_string()),
            (prompt, None) => prompt,
        };

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
        let pacing = zeroclaw_config::schema::PacingConfig::default();
        let loop_knobs = LoopKnobs::default();
        let execution = tokio::time::timeout(
            Duration::from_secs(agentic_timeout_secs),
            run_tool_call_loop(ToolLoop {
                sop_reassembly: None,
                exec: ResolvedAgentExecution::resolve(
                    ResolvedModelAccess {
                        model_provider,
                        provider_name: provider_type,
                        model,
                        temperature: effective_temperature,
                    },
                    ResolvedIo {
                        tools_registry: &sub_tools,
                        observer: &noop_observer,
                        silent: true,
                        approval: approval_manager.as_ref(),
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
                        receipt_generator,
                    },
                    ResolvedRuntimeKnobs {
                        max_tool_iterations: loop_runtime.max_tool_iterations,
                        excluded_tools: &[],
                        dedup_exempt_tools: tool_policy.excluded_tools.as_deref().unwrap_or(&[]),
                        pacing: &pacing,
                        strict_tool_parsing: loop_runtime.strict_tool_parsing,
                        parallel_tools: loop_runtime.parallel_tools,
                        max_tool_result_chars: loop_runtime.max_tool_result_chars,
                        // Keep delegate subagent context pruning aligned with top-level
                        // agents instead of preserving the old disabled-by-zero path.
                        context_token_budget: loop_runtime.max_context_tokens,
                        knobs: &loop_knobs,
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
        );
        let result = match thinking_params {
            Some(params) => {
                zeroclaw_api::NATIVE_THINKING_OVERRIDE
                    .scope(params.native_thinking, execution)
                    .await
            }
            None => execution.await,
        };

        match result {
            Ok(Ok(response)) if response.trim().is_empty() => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(invalid_semantic_completion_error(agent_name)),
            }),
            Ok(Ok(response)) => Ok(ToolResult {
                success: true,
                output: format!(
                    "[Agent '{agent_name}' ({provider_type}/{model}, agentic)]\n{response}",
                )
                .into(),
                error: None,
            }),
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(delegate_failure_error(agent_name, &e)),
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
    fn tool_provenance(&self) -> ::zeroclaw_api::attribution::ToolProvenance {
        self.inner.tool_provenance()
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

    // Forward `spec()` so inner overrides keep their `Arc`-shared parameter
    // schemas; the trait default would rebuild the spec from
    // `parameters_schema()`, deep-cloning MCP schemas every loop iteration.
    fn spec(&self) -> zeroclaw_api::tool::ToolSpec {
        self.inner.spec()
    }

    fn invocation_triggers(&self) -> Vec<String> {
        self.inner.invocation_triggers()
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
    use crate::platform::{NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::{MemoryRecallTool, MemoryStoreTool};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use tokio::time::{Instant, sleep};
    use zeroclaw_config::scattered_types::{ThinkingConfig, ThinkingLevel};
    use zeroclaw_config::schema::{
        Config, CustomModelProviderConfig, DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS,
        DEFAULT_DELEGATE_TIMEOUT_SECS, DelegateExecutionMode, DelegateTargetConfig,
        ModelProviderConfig, ModelRouteConfig,
    };
    use zeroclaw_memory::{AgentScopedMemory, SqliteMemory};
    use zeroclaw_providers::{
        ChatRequest, ChatResponse, ReliableProviderTerminalFailure,
        ReliableProviderTerminalFailureKind, ToolCall,
    };

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

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> PathBuf {
            std::env::temp_dir()
        }

        fn supports_long_running(&self) -> bool {
            false
        }

        fn shell_dialect(&self) -> crate::platform::ShellDialect {
            crate::platform::ShellDialect::Posix
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

    /// `EchoTool` under an MCP prefixed name (`<server>__<tool>`).
    ///
    /// The provider-fallback and text-protocol tests need the bounded target to
    /// actually receive a tool. A bare fixture name is unclassified and is now
    /// omitted by design, so these fixtures reach the target the way a real one
    /// does: the target's own `mcp_bundles` grant `echo_srv`.
    #[derive(Default)]
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

    #[derive(Default)]
    struct IndependentRiskPolicyModelProvider {
        tool_messages: std::sync::Mutex<Vec<String>>,
    }

    impl IndependentRiskPolicyModelProvider {
        fn tool_messages(&self) -> Vec<String> {
            self.tool_messages.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for IndependentRiskPolicyModelProvider {
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
            let tool_messages: Vec<String> = request
                .messages
                .iter()
                .filter(|message| message.role == "tool")
                .map(|message| message.content.clone())
                .collect();
            if !tool_messages.is_empty() {
                self.tool_messages.lock().unwrap().extend(tool_messages);
                return Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                });
            }

            Ok(ChatResponse {
                text: None,
                tool_calls: vec![
                    ToolCall {
                        id: "call_skill_rm".to_string(),
                        name: "rm_marker__remove".to_string(),
                        arguments: "{}".to_string(),
                        extra_content: None,
                    },
                    ToolCall {
                        id: "call_shell_rm".to_string(),
                        name: "shell".to_string(),
                        arguments:
                            r#"{"command":"rm independent-delegate-marker","approved":true}"#
                                .to_string(),
                        extra_content: None,
                    },
                ],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for IndependentRiskPolicyModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "IndependentRiskPolicyModelProvider"
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

    fn http_request_json(request: &[u8]) -> serde_json::Value {
        let body_start = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("captured HTTP request has a header terminator");
        serde_json::from_slice(&request[body_start..])
            .expect("captured HTTP request body is valid JSON")
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

    async fn start_failing_chat_server(
        status: u16,
    ) -> (LocalChatServer, Arc<std::sync::atomic::AtomicUsize>) {
        start_failing_chat_server_with_error(status, "synthetic primary failure").await
    }

    async fn start_failing_chat_server_with_error(
        status: u16,
        error_message: &'static str,
    ) -> (LocalChatServer, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let task = zeroclaw_spawn::spawn!(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _request = read_http_request(&mut socket).await;
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = format!(r#"{{"error":{{"message":"{error_message}"}}}}"#);
            let response = format!(
                "HTTP/1.1 {status} Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        (LocalChatServer { uri, _task: task }, requests)
    }

    async fn start_primary_failure_then_final_chat_server()
    -> (LocalChatServer, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let task = zeroclaw_spawn::spawn!(async move {
            let (mut first_socket, _) = listener.accept().await.unwrap();
            let _request = read_http_request(&mut first_socket).await;
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = r#"{"error":{"message":"synthetic primary failure"}}"#;
            let response = format!(
                "HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            first_socket.write_all(response.as_bytes()).await.unwrap();

            let (mut second_socket, _) = listener.accept().await.unwrap();
            let _request = read_http_request(&mut second_socket).await;
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            write_json_response(
                &mut second_socket,
                serde_json::json!({
                    "choices": [{"message": {"content": "final primary reply"}}]
                }),
            )
            .await;
        });

        (LocalChatServer { uri, _task: task }, requests)
    }

    async fn start_tool_call_chat_server() -> (LocalChatServer, Arc<std::sync::atomic::AtomicUsize>)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let task = zeroclaw_spawn::spawn!(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _request = read_http_request(&mut socket).await;
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            write_json_response(
                &mut socket,
                chat_completion_tool_call(
                    "echo_srv__echo_tool",
                    "fallback_tool",
                    serde_json::json!({"value": "ping"}),
                ),
            )
            .await;
        });

        (LocalChatServer { uri, _task: task }, requests)
    }

    async fn start_text_tool_then_final_chat_server() -> (
        LocalChatServer,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let request_bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_bodies = Arc::clone(&request_bodies);
        let task = zeroclaw_spawn::spawn!(async move {
            let responses = [
                serde_json::json!({
                    "choices": [{"message": {"content": "<tool_call>{\"name\":\"echo_srv__echo_tool\",\"arguments\":{\"value\":\"fallback\"}}</tool_call>"}}]
                }),
                serde_json::json!({
                    "choices": [{"message": {"content": "fallback final reply"}}]
                }),
            ];
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_http_request(&mut socket).await;
                request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                captured_bodies.lock().unwrap().push(request);
                write_json_response(&mut socket, response).await;
            }
        });

        (
            LocalChatServer { uri, _task: task },
            requests,
            request_bodies,
        )
    }

    async fn start_slow_chat_server(
        delay: Duration,
    ) -> (LocalChatServer, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let task = zeroclaw_spawn::spawn!(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _request = read_http_request(&mut socket).await;
            request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(delay).await;
        });

        (LocalChatServer { uri, _task: task }, requests)
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
    async fn background_agentic_delegate_persists_localized_semantic_empty_error() {
        // The configured Reliable wrapper retries a semantic-empty completion
        // twice before terminal delivery. Supply one empty response per
        // attempt so this boundary test proves the final semantic-empty cause,
        // not a connection error after the fixture exhausts its responses.
        let server = start_final_chat_server(vec!["", "", ""]).await;
        let fixture = delegate_memory_fixture(Some(server.uri.clone())).await;

        let result = fixture
            .tool
            .execute(json!({
                "agent": "target",
                "prompt": "return a final answer",
                "background": true
            }))
            .await
            .unwrap();

        assert!(result.success, "background task should start: {result:?}");
        let task_id = result
            .output
            .lines()
            .find(|line| line.starts_with("task_id:"))
            .expect("background task id")
            .trim_start_matches("task_id: ")
            .trim();
        let background = wait_for_terminal_background_result(&fixture.workspace_dir, task_id).await;

        assert_eq!(
            background.status,
            BackgroundTaskStatus::Failed,
            "{background:?}"
        );
        assert!(background.output.is_none(), "{background:?}");
        assert_eq!(
            background.error.as_deref(),
            Some(invalid_semantic_completion_error("target").as_str()),
            "{background:?}"
        );
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

        let model_provider = ToolCountModelProvider { expected_tools: 0 };
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
    async fn non_agentic_delegate_rejects_empty_terminal_completion() {
        let result = DelegateTool::render_non_agentic_result(
            "delegate",
            "test-provider",
            "test-model",
            Ok(" \n\t".to_string()),
        );

        assert!(!result.success, "empty terminal completion must fail");
        assert!(
            result.output.is_empty(),
            "failed delegate must not emit output"
        );
        let error = result.error.as_deref().unwrap_or_default();
        assert_eq!(error, invalid_semantic_completion_error("delegate"));
        assert!(!error.contains("[Empty response]"), "{error}");
    }

    #[tokio::test]
    async fn non_agentic_delegate_rejects_think_only_terminal_completion() {
        let result = DelegateTool::render_non_agentic_result(
            "delegate",
            "test-provider",
            "test-model",
            Ok("<think>internal reasoning</think>".to_string()),
        );

        assert!(!result.success, "think-only terminal completion must fail");
        assert!(
            result.output.is_empty(),
            "failed delegate must not emit output"
        );
        let error = result.error.as_deref().unwrap_or_default();
        assert_eq!(error, invalid_semantic_completion_error("delegate"));
    }

    #[tokio::test]
    async fn non_agentic_delegate_projects_typed_terminal_failure() {
        let result = DelegateTool::render_non_agentic_result(
            "delegate",
            "test-provider",
            "test-model",
            Err(anyhow::Error::new(
                zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion,
            )),
        );

        assert!(!result.success);
        assert!(result.output.is_empty());
        let expected = invalid_semantic_completion_error("delegate");
        assert_eq!(result.error.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn non_agentic_delegate_retains_provider_terminal_diagnostic() {
        let result = DelegateTool::render_non_agentic_result(
            "delegate",
            "custom",
            "model",
            Err(anyhow::Error::new(ReliableProviderTerminalFailure::new(
                ReliableProviderTerminalFailureKind::Connection,
                None,
                "All model providers/models failed after 3 failure event(s). Events: retry 1/3"
                    .to_string(),
            ))),
        );

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some(
                "Agent 'delegate' failed: All model providers/models failed after 3 failure event(s). Events: retry 1/3"
            )
        );
    }

    #[test]
    fn delegate_failure_projects_provider_tools_terminal_category() {
        let error = anyhow::Error::new(
            crate::agent::turn::outcome::StreamPreExecutedToolsWithoutFinalResponse { usage: None },
        );
        let expected = crate::agent::turn::outcome::terminal_completion_error_message(
            &error,
            Some("delegate"),
        )
        .expect("provider-tools terminal category must project");

        assert_eq!(delegate_failure_error("delegate", &error), expected);
        assert_ne!(
            expected,
            "Agent 'delegate' failed: provider stream ended after provider-executed tools without a final response"
        );
    }

    #[tokio::test]
    async fn execute_agentic_rejects_empty_terminal_completion() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(Vec::new()))
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "test-provider",
                "test-model",
                &EmptyTerminalModelProvider,
                "run",
                Some(0.2),
            )
            .await
            .expect("delegate returns a failed tool result rather than an error");

        assert!(!result.success, "empty terminal completion must fail");
        assert!(
            result.output.is_empty(),
            "failed delegate must not emit output"
        );
        let error = result.error.as_deref().unwrap_or_default();
        assert_eq!(error, invalid_semantic_completion_error("agentic"));
        assert!(!error.contains("[Empty response]"), "{error}");
    }

    #[tokio::test]
    async fn execute_agentic_rejects_empty_completion_after_tool_result() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec!["echo_tool".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "test-provider",
                "test-model",
                &ToolThenEmptyTerminalModelProvider,
                "run",
                Some(0.2),
            )
            .await
            .expect("delegate returns a failed tool result rather than an error");

        assert!(!result.success, "empty terminal completion must fail");
        assert!(
            result.output.is_empty(),
            "failed delegate must not emit output"
        );
        let error = result.error.as_deref().unwrap_or_default();
        assert_eq!(error, invalid_semantic_completion_error("agentic"));
        assert!(!error.contains("[Empty response]"), "{error}");
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

    struct EmptyTerminalModelProvider;

    struct ToolThenEmptyTerminalModelProvider;

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

    #[async_trait]
    impl ModelProvider for EmptyTerminalModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: None,
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: Some("reasoning only".to_string()),
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for EmptyTerminalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "EmptyTerminalModelProvider"
        }
    }

    #[async_trait]
    impl ModelProvider for ToolThenEmptyTerminalModelProvider {
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
            anyhow::bail!("unused")
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if request
                .messages
                .iter()
                .any(|message| message.role == "tool")
            {
                return Ok(ChatResponse {
                    text: None,
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: Some("reasoning only".to_string()),
                });
            }

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

    impl ::zeroclaw_api::attribution::Attributable for ToolThenEmptyTerminalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ToolThenEmptyTerminalModelProvider"
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

    #[derive(Debug)]
    struct RecordedThinkingRequest {
        thinking_budget: Option<u32>,
        thinking_display: Option<zeroclaw_api::model_provider::ThinkingDisplay>,
        system_prompt: Option<String>,
        temperature: Option<f64>,
    }

    #[derive(Default)]
    struct ThinkingRecordingModelProvider {
        requests: std::sync::Mutex<Vec<RecordedThinkingRequest>>,
    }

    impl ThinkingRecordingModelProvider {
        fn request(&self) -> RecordedThinkingRequest {
            let mut requests = self.requests.lock().unwrap();
            assert_eq!(requests.len(), 1, "expected exactly one provider request");
            requests.pop().unwrap()
        }
    }

    #[async_trait]
    impl ModelProvider for ThinkingRecordingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("tool loop must use ChatRequest")
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let thinking = request.thinking.as_ref();
            let thinking_budget = thinking.map(|params| params.budget_tokens);
            let thinking_display = thinking.and_then(|params| params.display);
            let system_prompt = request
                .messages
                .iter()
                .find(|message| message.role == "system")
                .map(|message| message.content.clone());
            self.requests.lock().unwrap().push(RecordedThinkingRequest {
                thinking_budget,
                thinking_display,
                system_prompt,
                temperature,
            });
            Ok(ChatResponse {
                text: Some("delegate complete".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ThinkingRecordingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "ThinkingRecordingModelProvider"
        }
    }

    fn thinking_delegate_fixture(
        mode: DelegateExecutionMode,
        thinking: ThinkingConfig,
    ) -> (DelegateTool, AliasedAgentConfig) {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};

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
                ..RiskProfileConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("target".to_string(), RiskProfileConfig::default());
        config.runtime_profiles.insert(
            "target-runtime".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                thinking,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                risk_profile: "caller".into(),
                model_provider: "custom.unused".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "target".to_string(),
                    mode,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        let target = AliasedAgentConfig {
            risk_profile: "target".into(),
            runtime_profile: "target-runtime".into(),
            model_provider: "custom.unused".into(),
            ..AliasedAgentConfig::default()
        };
        config.agents.insert("target".to_string(), target.clone());

        let config = Arc::new(config);
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, caller_policy)
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(DelegateTestRuntime))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone());
        (tool, target)
    }

    #[tokio::test]
    async fn independent_delegate_uses_target_native_thinking_and_restores_parent_scope() {
        let target_thinking = ThinkingConfig {
            default_level: ThinkingLevel::Max,
            native_thinking: true,
            ..ThinkingConfig::default()
        };
        let (tool, target) =
            thinking_delegate_fixture(DelegateExecutionMode::Independent, target_thinking);
        let provider = ThinkingRecordingModelProvider::default();
        let parent = Some(zeroclaw_config::scattered_types::NativeThinkingParams {
            budget_tokens: 10_000,
            display: None,
        });

        zeroclaw_api::NATIVE_THINKING_OVERRIDE
            .scope(parent, async {
                let result = tool
                    .execute_agentic(
                        "target",
                        &target,
                        "custom",
                        "test-model",
                        &provider,
                        "analyze this",
                        Some(0.2),
                    )
                    .await
                    .unwrap();
                assert!(result.success, "{result:?}");
                assert_eq!(
                    zeroclaw_api::NATIVE_THINKING_OVERRIDE
                        .try_with(Clone::clone)
                        .unwrap(),
                    parent,
                    "the target scope must not leak into the parent turn"
                );
            })
            .await;

        let request = provider.request();
        assert_eq!(request.thinking_budget, Some(50_000));
        assert!(
            request
                .temperature
                .is_some_and(|temperature| (temperature - 0.3).abs() < f64::EPSILON)
        );
    }

    #[tokio::test]
    async fn independent_delegate_propagates_display_mode_to_target_request() {
        use zeroclaw_api::model_provider::ThinkingDisplay;
        use zeroclaw_config::scattered_types::ThinkingDisplayMode;

        let target_thinking = ThinkingConfig {
            default_level: ThinkingLevel::Max,
            native_thinking: true,
            display: ThinkingDisplayMode::Updates,
            ..ThinkingConfig::default()
        };
        let (tool, target) =
            thinking_delegate_fixture(DelegateExecutionMode::Independent, target_thinking);
        let provider = ThinkingRecordingModelProvider::default();
        let parent = Some(zeroclaw_config::scattered_types::NativeThinkingParams {
            budget_tokens: 10_000,
            display: None,
        });

        zeroclaw_api::NATIVE_THINKING_OVERRIDE
            .scope(parent, async {
                let result = tool
                    .execute_agentic(
                        "target",
                        &target,
                        "custom",
                        "test-model",
                        &provider,
                        "analyze this",
                        Some(0.2),
                    )
                    .await
                    .unwrap();
                assert!(result.success, "{result:?}");
            })
            .await;

        let request = provider.request();
        assert_eq!(
            request.thinking_display,
            Some(ThinkingDisplay::Updates),
            "the target agent's display mode must reach the provider request"
        );
    }

    #[tokio::test]
    async fn independent_delegate_prepends_target_non_native_thinking_prompt() {
        let target_thinking = ThinkingConfig {
            default_level: ThinkingLevel::Max,
            native_thinking: false,
            ..ThinkingConfig::default()
        };
        let (tool, target) =
            thinking_delegate_fixture(DelegateExecutionMode::Independent, target_thinking);
        let provider = ThinkingRecordingModelProvider::default();
        let parent = Some(zeroclaw_config::scattered_types::NativeThinkingParams {
            budget_tokens: 10_000,
            display: None,
        });

        zeroclaw_api::NATIVE_THINKING_OVERRIDE
            .scope(parent, async {
                let result = tool
                    .execute_agentic(
                        "target",
                        &target,
                        "custom",
                        "test-model",
                        &provider,
                        "analyze this",
                        Some(0.2),
                    )
                    .await
                    .unwrap();
                assert!(result.success, "{result:?}");
                assert_eq!(
                    zeroclaw_api::NATIVE_THINKING_OVERRIDE
                        .try_with(Clone::clone)
                        .unwrap(),
                    parent,
                    "a non-native target must clear the override only inside its child scope"
                );
            })
            .await;

        let request = provider.request();
        assert_eq!(request.thinking_budget, None);
        assert!(
            request
                .system_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.starts_with("Think very carefully and exhaustively.")),
            "target thinking prefix must precede the delegated system prompt: {request:?}"
        );
        assert!(
            request
                .temperature
                .is_some_and(|temperature| (temperature - 0.3).abs() < f64::EPSILON)
        );
    }

    #[tokio::test]
    async fn bounded_delegate_retains_parent_thinking_scope() {
        let target_thinking = ThinkingConfig {
            default_level: ThinkingLevel::Max,
            native_thinking: true,
            ..ThinkingConfig::default()
        };
        let (tool, target) =
            thinking_delegate_fixture(DelegateExecutionMode::Bounded, target_thinking);
        let provider = ThinkingRecordingModelProvider::default();
        let parent = Some(zeroclaw_config::scattered_types::NativeThinkingParams {
            budget_tokens: 10_000,
            display: None,
        });

        zeroclaw_api::NATIVE_THINKING_OVERRIDE
            .scope(parent, async {
                let result = tool
                    .execute_agentic(
                        "target",
                        &target,
                        "custom",
                        "test-model",
                        &provider,
                        "analyze this",
                        Some(0.2),
                    )
                    .await
                    .unwrap();
                assert!(result.success, "{result:?}");
            })
            .await;

        let request = provider.request();
        assert_eq!(request.thinking_budget, Some(10_000));
        assert_eq!(request.temperature, Some(0.2));
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
            security: Arc::new(zeroclaw_config::policy::SecurityPolicy::default()),
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
    fn enriched_prompt_resolves_explicit_global_full_skill_mode() {
        let mut root_config = Config::default();
        root_config.skills.prompt_injection_mode =
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full;
        root_config
            .agents
            .insert("alpha".into(), AliasedAgentConfig::default());
        let root_config = Arc::new(root_config);
        let config = root_config.agents.get("alpha").unwrap().clone();
        let workspace = std::env::temp_dir();
        let tools: Vec<Box<dyn Tool>> = vec![];
        let skills = vec![crate::skills::Skill {
            name: "deploy".into(),
            description: "Release safely".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec!["Run <smoke> & release checks.".into()],
            slash_options: Vec::new(),
            always: false,
            location: None,
        }];

        let tool = DelegateTool::new(root_config.agents.clone(), None, test_security())
            .with_root_config(root_config)
            .with_workspace_dir(workspace.to_path_buf());
        assert_eq!(
            tool.resolve_loop_runtime("alpha", &config)
                .prompt_injection_mode,
            zeroclaw_config::schema::SkillsPromptInjectionMode::Full
        );
        let prompt = tool
            .build_enriched_system_prompt(
                "alpha",
                &config,
                "test-model",
                &tools,
                &workspace,
                false,
                Some(&skills),
            )
            .unwrap();

        assert!(prompt.contains("<instructions>"));
        assert!(
            prompt.contains("<instruction>Run &lt;smoke&gt; &amp; release checks.</instruction>")
        );
        assert!(!prompt.contains("read_skill"));
        assert!(!prompt.contains("loaded on demand"));
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

        struct PosixRuntime;
        impl crate::platform::RuntimeAdapter for PosixRuntime {
            fn name(&self) -> &str {
                "posix-test"
            }
            fn has_filesystem_access(&self) -> bool {
                true
            }
            fn storage_path(&self) -> std::path::PathBuf {
                std::env::temp_dir()
            }
            fn supports_long_running(&self) -> bool {
                false
            }
            fn shell_dialect(&self) -> crate::platform::ShellDialect {
                crate::platform::ShellDialect::Posix
            }
            fn build_shell_command(
                &self,
                command: &str,
                workspace_dir: &std::path::Path,
            ) -> anyhow::Result<tokio::process::Command> {
                let mut cmd = tokio::process::Command::new("/bin/sh");
                cmd.args(["-c", command]).current_dir(workspace_dir);
                Ok(cmd)
            }
        }

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockShellTool)];
        let workspace = std::env::temp_dir();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.to_path_buf())
            .with_runtime(Arc::new(PosixRuntime));

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
            prompt.contains("## Shell"),
            "should contain shell section when shell tool is present"
        );
        assert!(
            !prompt.contains("## Shell Policy"),
            "static shell policy block must not appear"
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
    fn enriched_prompt_reports_powershell_dialect_for_delegate_with_shell_tool() {
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

        struct PsRuntime;
        impl crate::platform::RuntimeAdapter for PsRuntime {
            fn name(&self) -> &str {
                "ps-test"
            }
            fn has_filesystem_access(&self) -> bool {
                true
            }
            fn storage_path(&self) -> std::path::PathBuf {
                std::env::temp_dir()
            }
            fn supports_long_running(&self) -> bool {
                false
            }
            fn shell_dialect(&self) -> crate::platform::ShellDialect {
                crate::platform::ShellDialect::PowerShell
            }
            fn shell_profile(&self) -> Option<zeroclaw_api::runtime_traits::ShellProfile> {
                Some(zeroclaw_api::runtime_traits::ShellProfile {
                    name: "powershell".to_string(),
                    dialect: crate::platform::ShellDialect::PowerShell,
                })
            }
            fn build_shell_command(
                &self,
                command: &str,
                workspace_dir: &std::path::Path,
            ) -> anyhow::Result<tokio::process::Command> {
                let mut cmd = tokio::process::Command::new("powershell");
                cmd.args(["-Command", command]).current_dir(workspace_dir);
                Ok(cmd)
            }
        }

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockShellTool)];
        let workspace = std::env::temp_dir();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.to_path_buf())
            .with_runtime(Arc::new(PsRuntime));

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
            prompt.contains("Shell: powershell") || prompt.contains("powershell"),
            "prompt must identify powershell dialect; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("trash"),
            "POSIX deletion advice must not appear in a PowerShell delegate prompt"
        );
        assert!(
            prompt.contains("Get-ChildItem") || prompt.contains("Remove-Item"),
            "PowerShell deletion guidance must appear; got:\n{prompt}"
        );
        assert!(
            !prompt.contains("## Shell Policy"),
            "static shell policy block must not appear"
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
    async fn bounded_delegate_with_different_risk_profile_keeps_own_workspace_dir() {
        // A Bounded delegate whose risk profile DIFFERS from the caller's must
        // NOT inherit the caller's session workspace - only same-profile
        // Bounded delegates should (covered above). Mirrors the reported
        // executive_assistant("balanced") -> researcher("research") config.
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

        let root = std::env::temp_dir().join(format!(
            "zeroclaw-delegate-diff-profile-{}",
            uuid::Uuid::new_v4()
        ));
        let mut config = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "balanced".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("research".to_string(), RiskProfileConfig::default());
        config.agents.insert(
            "executive_assistant".to_string(),
            AliasedAgentConfig {
                risk_profile: "balanced".into(),
                model_provider: "ollama.executive_assistant".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "researcher".to_string(),
                    mode: DelegateExecutionMode::Bounded,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                risk_profile: "research".into(),
                model_provider: "ollama.researcher".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);

        let session_cwd = PathBuf::from("/tmp/zeroclaw-test-diff-profile-session-cwd");
        let mut caller_policy = SecurityPolicy::for_agent(&config, "executive_assistant")
            .expect("caller policy resolves");
        caller_policy.workspace_dir = session_cwd.clone();
        let caller_policy = Arc::new(caller_policy);

        let target_config_workspace = config.agent_workspace_dir("researcher");
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
            .with_caller_alias("executive_assistant");

        let target_policy = tool
            .policy_for_target("researcher")
            .expect("different-profile bounded target resolves");
        assert_eq!(
            target_policy.workspace_dir, target_config_workspace,
            "bounded delegate with a DIFFERENT risk profile must keep its own \
             configured workspace, not inherit the caller's session cwd \
             (issue #9872): got {:?}, expected {:?}",
            target_policy.workspace_dir, target_config_workspace
        );
    }

    struct BoundedDelegateFsFixture {
        _tmp: TempDir,
        tool: DelegateTool,
        target_config: AliasedAgentConfig,
        caller_workspace: PathBuf,
        target_workspace: PathBuf,
        config: Arc<Config>,
    }

    /// Builds a caller ("executive_assistant", risk profile "balanced") that bounded-delegates
    /// to a target ("fs_researcher", risk profile "research") whose `parent_tools` entry for
    /// `tool_name` is the REAL production tool, built via `default_tools_with_runtime` (the
    /// same factory the live runtime uses) and bound to the CALLER's session workspace. This
    /// mirrors exactly how `DelegateTool::execute_agentic_with_admission`'s `Bounded` branch
    /// (delegate.rs ~2644-2687) assembles `sub_tools` in production, so it exercises the
    /// real failing path - unlike a `policy_for_target()`-only assertion.
    async fn bounded_delegate_fs_fixture(
        tool_name: &str,
        runtime: Arc<dyn RuntimeAdapter>,
    ) -> BoundedDelegateFsFixture {
        bounded_delegate_fs_fixture_with_config(tool_name, runtime, |_| {}).await
    }

    /// Same as [`bounded_delegate_fs_fixture`], but lets the caller tweak the
    /// assembled `Config` before it's frozen into an `Arc` (e.g. to set the
    /// target's `sandbox_backend`/`sandbox_enabled` on its `"research"` risk
    /// profile) - mirrors the `configure` closure already used by
    /// `bounded_delegate_full_fixture` below.
    async fn bounded_delegate_fs_fixture_with_config(
        tool_name: &str,
        runtime: Arc<dyn RuntimeAdapter>,
        configure: impl FnOnce(&mut Config),
    ) -> BoundedDelegateFsFixture {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};

        let tmp = TempDir::new().unwrap();
        let caller_workspace = tmp.path().join("caller-session-cwd");
        std::fs::create_dir_all(&caller_workspace).unwrap();

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        // allowed_commands/block_high_risk_commands only matter for the "shell" variant;
        // harmless wildcards for the "file_write" variant.
        config.risk_profiles.insert(
            "balanced".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec![tool_name.to_string(), "delegate".to_string()],
                allowed_commands: vec!["*".to_string()],
                block_high_risk_commands: false,
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "research".to_string(),
            RiskProfileConfig {
                allowed_tools: vec![tool_name.to_string()],
                allowed_commands: vec!["*".to_string()],
                block_high_risk_commands: false,
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "fs_agentic_test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 5,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "executive_assistant".to_string(),
            AliasedAgentConfig {
                risk_profile: "balanced".into(),
                runtime_profile: "fs_agentic_test".into(),
                model_provider: "ollama.executive_assistant".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "fs_researcher".to_string(),
                    mode: DelegateExecutionMode::Bounded,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        let target_config = AliasedAgentConfig {
            risk_profile: "research".into(),
            runtime_profile: "fs_agentic_test".into(),
            model_provider: "ollama.fs_researcher".into(),
            ..AliasedAgentConfig::default()
        };
        config
            .agents
            .insert("fs_researcher".to_string(), target_config.clone());

        let target_workspace = config.agent_workspace_dir("fs_researcher");
        assert_ne!(
            caller_workspace, target_workspace,
            "test precondition: caller session cwd must differ from the target's configured workspace"
        );

        configure(&mut config);
        let config = Arc::new(config);

        let mut caller_policy = SecurityPolicy::for_agent(&config, "executive_assistant")
            .expect("caller policy resolves");
        // Caller's actual session cwd, which can legitimately differ from its own
        // configured agent workspace (e.g. a dynamic per-turn session dir) - mirrors
        // the reported executive_assistant/researcher configuration.
        caller_policy.workspace_dir = caller_workspace.clone();
        let caller_policy = Arc::new(caller_policy);

        // Build parent_tools from the SAME factory production uses
        // (`default_tools_with_runtime`), so this fixture cannot silently drift from
        // what a real caller turn actually assembles.
        let caller_tool_registry = crate::tools::default_tools_with_runtime(
            Arc::clone(&caller_policy),
            Arc::clone(&runtime),
        );
        let real_tool = caller_tool_registry
            .into_iter()
            .find(|t| t.name() == tool_name)
            .unwrap_or_else(|| panic!("default_tools_with_runtime did not register '{tool_name}'"));
        let parent_tools: Vec<Arc<dyn Tool>> = vec![Arc::from(real_tool)];

        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }

        // `.with_runtime(...)` mirrors production (tools/mod.rs always sets it when
        // wiring a live DelegateTool) - the Bounded branch's fs-tool rebuild needs it
        // to construct the target's own instances via the same canonical factory.
        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_workspace_dir(caller_workspace.clone())
            .with_parent_tools(Arc::new(RwLock::new(parent_tools)))
            .with_runtime(runtime)
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_caller_alias("executive_assistant");

        BoundedDelegateFsFixture {
            _tmp: tmp,
            tool,
            target_config,
            caller_workspace,
            target_workspace,
            config,
        }
    }

    struct BoundedFileWriteThenFinalModelProvider {
        path: &'static str,
        content: &'static str,
    }

    #[async_trait]
    impl ModelProvider for BoundedFileWriteThenFinalModelProvider {
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
            if tool_message_count == 0 {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_write".to_string(),
                        name: "file_write".to_string(),
                        arguments: serde_json::json!({
                            "path": self.path,
                            "content": self.content
                        })
                        .to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: Some("bounded fs delegate done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for BoundedFileWriteThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "BoundedFileWriteThenFinalModelProvider"
        }
    }

    struct BoundedShellThenFinalModelProvider {
        command: &'static str,
    }

    #[async_trait]
    impl ModelProvider for BoundedShellThenFinalModelProvider {
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
            if tool_message_count == 0 {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_shell".to_string(),
                        name: "shell".to_string(),
                        arguments: serde_json::json!({
                            "command": self.command,
                            "approved": true
                        })
                        .to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: Some("bounded shell delegate done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for BoundedShellThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "BoundedShellThenFinalModelProvider"
        }
    }

    /// Real regression test: drives a REAL bounded delegate turn (through
    /// `execute_agentic`, hitting the `DelegateExecutionMode::Bounded` branch at
    /// delegate.rs ~2644-2687) with a target whose risk profile DIFFERS from the
    /// caller's, and asserts on the actual filesystem side effect of a real
    /// `file_write` tool call - not just on `policy_for_target()` in isolation.
    #[tokio::test]
    async fn bounded_delegate_file_write_lands_in_target_workspace_not_callers() {
        let fixture =
            bounded_delegate_fs_fixture("file_write", Arc::new(DelegateTestRuntime)).await;
        let model_provider = BoundedFileWriteThenFinalModelProvider {
            path: "proof.txt",
            content: "written by bounded fs target",
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "write a proof file",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "bounded fs delegate failed: {result:?}");

        assert!(
            !fixture.caller_workspace.join("proof.txt").exists(),
            "regression for #9872: a Bounded delegate's file_write must NOT land in the \
             caller's session workspace ({}) just because its tool instance was reused from \
             parent_tools",
            fixture.caller_workspace.display()
        );
        let target_file = fixture.target_workspace.join("proof.txt");
        assert!(
            target_file.exists(),
            "regression for #9872: a Bounded delegate's file_write must land in the TARGET's \
             own configured workspace ({}), not the caller's",
            fixture.target_workspace.display()
        );
        assert_eq!(
            tokio::fs::read_to_string(&target_file).await.unwrap(),
            "written by bounded fs target"
        );
    }

    /// Same regression as above, through the `shell` tool instead of `file_write`. Uses the
    /// REAL `NativeRuntime` (not the fake `DelegateTestRuntime`) so the command actually
    /// executes and its cwd can be observed via a real created file - `DelegateTestRuntime`'s
    /// `build_shell_command` only echoes the command string back and never runs a real shell,
    /// so it cannot prove *where* a command executed.
    #[tokio::test]
    async fn bounded_delegate_shell_command_executes_in_target_workspace_not_callers() {
        let fixture =
            bounded_delegate_fs_fixture("shell", Arc::new(crate::platform::NativeRuntime::new()))
                .await;
        let model_provider = BoundedShellThenFinalModelProvider {
            command: "echo written-by-bounded-shell-target > shell_proof.txt",
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "run a proof command",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "bounded shell delegate failed: {result:?}");

        assert!(
            !fixture.caller_workspace.join("shell_proof.txt").exists(),
            "regression for #9872: a Bounded delegate's shell command must NOT execute with \
             the caller's session workspace ({}) as its cwd",
            fixture.caller_workspace.display()
        );
        assert!(
            fixture.target_workspace.join("shell_proof.txt").exists(),
            "regression for #9872: a Bounded delegate's shell command must execute with the \
             TARGET's own configured workspace ({}) as its cwd, not the caller's",
            fixture.target_workspace.display()
        );
    }

    // ── Follow-up regressions: maintainer-review findings on this PR ────────
    //
    // The two tests above only prove WHERE a rebuilt tool's effect lands.
    // They do not prove the rebuilt `ShellTool` keeps the target's own OS
    // sandbox (it doesn't - `default_tools_with_runtime`, tools/mod.rs:300-349,
    // always builds `ShellTool::new`, which hardcodes `NoopSandbox`,
    // shell.rs:103-113), and `FILESYSTEM_TOOL_NAMES` only covers the 8 tools
    // that factory builds - `git_operations`, `backup`, and `data_management`
    // (among others) capture `workspace_dir` the exact same way but are not on
    // that list, so they still fall through to the caller's `ToolArcRef`
    // fallback (delegate.rs:2720-2732).

    /// Minimal `DelegateTool` wired with a `root_config`, enough to exercise
    /// `rebuild_target_shell_tool` directly - the SAME method the `Bounded`
    /// branch of `execute_agentic` calls, so these tests prove the actual
    /// wiring (not a hand-rebuilt approximation of it), without needing a
    /// full caller/target/model-provider fixture.
    fn delegate_tool_for_shell_rebuild_tests(shell_timeout_secs: u64) -> DelegateTool {
        let mut root_config = zeroclaw_config::schema::Config::default();
        root_config.shell_tool.timeout_secs = shell_timeout_secs;
        DelegateTool::new(HashMap::new(), None, test_security())
            .with_root_config(Arc::new(root_config))
    }

    #[test]
    fn bounded_delegate_shell_loses_targets_configured_sandbox() {
        // Regression: a Bounded delegate target explicitly configured with
        // `sandbox_backend = "docker"` must keep that OS-level sandbox when
        // its shell tool is rebuilt for a cross-profile delegation via
        // `DelegateTool::rebuild_target_shell_tool` - today it silently gets
        // `NoopSandbox` instead, because `default_tools_with_runtime` never
        // reads `SecurityPolicy.sandbox_backend`/`sandbox_enabled` at all.
        //
        // The oracle (`sandbox_posture`) is queried first and the test skips
        // itself when Docker isn't available in this environment, rather than
        // asserting against a specific backend name that only a real,
        // installed sandbox can produce - mirrors why the project's own
        // Landlock coverage lives in a separate, environment-gated CI job
        // (`Test (Landlock)`) instead of the default `cargo test` run.
        let tmp = TempDir::new().unwrap();
        let target_policy = Arc::new(SecurityPolicy {
            sandbox_backend: Some("docker".to_string()),
            sandbox_enabled: Some(true),
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });

        let posture = crate::security::detect::sandbox_posture(
            &target_policy.sandbox_config(),
            zeroclaw_config::schema::RuntimeKind::Native,
            Some(&target_policy.workspace_dir),
            &crate::security::SandboxExtraRoots {
                read_write: target_policy.allowed_roots.clone(),
                read_only: target_policy.allowed_roots_read_only.clone(),
                write_only: target_policy.allowed_roots_write_only.clone(),
            },
        );
        if posture.active_backend == "none" {
            eprintln!(
                "skipping bounded_delegate_shell_loses_targets_configured_sandbox: \
                 no Docker available in this environment (`docker --version` failed) - \
                 this regression is only observable where at least one real sandbox \
                 backend is installed"
            );
            return;
        }

        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(crate::platform::NativeRuntime::new());
        let delegate = delegate_tool_for_shell_rebuild_tests(60);
        let actual = delegate
            .rebuild_target_shell_tool(Arc::clone(&target_policy), runtime)
            .expect("rebuild_target_shell_tool should succeed with a root_config configured")
            .sandbox_name()
            .to_string();

        assert_eq!(
            actual, posture.active_backend,
            "regression: a Bounded delegate target's own configured OS sandbox \
             ({}) must be attached to its rebuilt shell tool by \
             DelegateTool::rebuild_target_shell_tool, not silently replaced with NoopSandbox",
            posture.active_backend
        );
    }

    #[test]
    fn bounded_delegate_shell_explicit_sandbox_disable_still_resolves_to_noop() {
        // Control case for the test above: an explicitly DISABLED sandbox
        // must keep resolving to NoopSandbox once the sandbox-resolution fix
        // lands too - this must never start failing as a side effect of
        // fixing the regression above.
        let tmp = TempDir::new().unwrap();
        let target_policy = Arc::new(SecurityPolicy {
            sandbox_backend: Some("docker".to_string()),
            sandbox_enabled: Some(false),
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(crate::platform::NativeRuntime::new());
        let delegate = delegate_tool_for_shell_rebuild_tests(60);
        let actual = delegate
            .rebuild_target_shell_tool(Arc::clone(&target_policy), runtime)
            .expect("rebuild_target_shell_tool should succeed with a root_config configured")
            .sandbox_name()
            .to_string();
        assert_eq!(
            actual, "none",
            "sandbox_enabled = Some(false) must always resolve to NoopSandbox"
        );
    }

    #[test]
    fn bounded_delegate_shell_zero_timeout_does_not_inherit_global_default() {
        // Regression (Warning-level finding): production resolves
        // `shell_timeout_secs == 0` to `root_config.shell_tool.timeout_secs`
        // (tools/mod.rs) - the documented "0 means inherit the global
        // timeout" contract (zeroclaw-config/src/schema.rs). A target's
        // rebuilt shell tool must honor the same contract via
        // `DelegateTool::rebuild_target_shell_tool`; today it doesn't,
        // because `ShellTool::new` (via `default_tools_with_runtime`) takes
        // `security.shell_timeout_secs` verbatim (shell.rs).
        let tmp = TempDir::new().unwrap();
        let target_policy = Arc::new(SecurityPolicy {
            shell_timeout_secs: 0,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(crate::platform::NativeRuntime::new());
        let delegate = delegate_tool_for_shell_rebuild_tests(42);
        let timeout = delegate
            .rebuild_target_shell_tool(Arc::clone(&target_policy), runtime)
            .expect("rebuild_target_shell_tool should succeed with a root_config configured")
            .timeout_secs();

        assert_eq!(
            timeout, 42,
            "regression: a Bounded delegate target whose shell_timeout_secs is 0 must \
             inherit the root_config's global default timeout (42), not literally run \
             with a 0-second timeout"
        );
    }

    struct BoundedSingleToolCallThenFinalModelProvider {
        tool_name: &'static str,
        tool_args: serde_json::Value,
    }

    #[async_trait]
    impl ModelProvider for BoundedSingleToolCallThenFinalModelProvider {
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
            let tool_messages: Vec<&str> = request
                .messages
                .iter()
                .filter(|m| m.role == "tool")
                .map(|m| m.content.as_str())
                .collect();
            if tool_messages.is_empty() {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: self.tool_name.to_string(),
                        arguments: self.tool_args.to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                // Echo the tool's ACTUAL result content back as the final text
                // (prefixed with plain prose - a raw JSON-shaped final message
                // trips the turn's own tool-call-envelope safety net, which
                // replaces it with a generic "internal tool-call format error"
                // notice), so tests can assert on what the tool itself
                // reported rather than on the overall turn's success (which
                // this mock always drives to completion regardless of the
                // tool's actual outcome).
                Ok(ChatResponse {
                    text: Some(format!("Tool reported: {}", tool_messages.join(" | "))),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for BoundedSingleToolCallThenFinalModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "BoundedSingleToolCallThenFinalModelProvider"
        }
    }

    /// Real regression test: proves `DelegateTool::execute_agentic`'s `Bounded`
    /// branch actually attaches the target's OWN configured OS sandbox to its
    /// rebuilt shell tool through the real path - not just that
    /// `rebuild_target_shell_tool` CAN produce a sandboxed tool in isolation
    /// (the 3 tests above this fixture's definitions). That distinction
    /// matters: a policy-only assertion already turned out insufficient once
    /// in this same issue, per the maintainer's own review.
    ///
    /// Docker's real sandbox bind-mounts the target workspace READ-ONLY
    /// (`security/docker.rs`'s `wrap_command`, `-v ...:ro`) and replaces the
    /// entire command (including the host's native shell-dialect wrapper)
    /// with `docker run ... <image> <original argv>`. So once the sandbox is
    /// genuinely attached through this real `execute_agentic` path, a shell
    /// command that writes inside its own workspace must fail - on Linux this
    /// is the read-only mount rejecting the write; on a non-Linux host it can
    /// instead be the native dialect wrapper (e.g. `cmd.exe`) not existing
    /// inside the Linux container image. Either way the file must never be
    /// created, unlike today's bug (`NoopSandbox`), where the write succeeds.
    #[tokio::test]
    async fn bounded_delegate_shell_command_uses_targets_configured_sandbox() {
        let probe_tmp = TempDir::new().unwrap();
        let posture = crate::security::detect::sandbox_posture(
            &zeroclaw_config::schema::SandboxConfig {
                enabled: Some(true),
                backend: zeroclaw_config::schema::SandboxBackend::Docker,
                firejail_args: vec![],
            },
            zeroclaw_config::schema::RuntimeKind::Native,
            Some(probe_tmp.path()),
            &crate::security::SandboxExtraRoots::default(),
        );
        if posture.active_backend != "docker" {
            eprintln!(
                "skipping bounded_delegate_shell_command_uses_targets_configured_sandbox: \
                 no Docker available in this environment (`docker --version` failed) - this \
                 regression is only observable where Docker is installed"
            );
            return;
        }

        let fixture = bounded_delegate_fs_fixture_with_config(
            "shell",
            Arc::new(crate::platform::NativeRuntime::new()),
            |config| {
                let research = config
                    .risk_profiles
                    .get_mut("research")
                    .expect("fixture always registers a 'research' risk profile");
                research.sandbox_backend = Some("docker".to_string());
                research.sandbox_enabled = Some(true);
            },
        )
        .await;

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "shell",
            tool_args: serde_json::json!({
                "command": "echo written-by-bounded-shell-target > shell_proof.txt",
                "approved": true
            }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "run a proof command",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "the bounded delegate turn itself must still complete (this mock always drives \
             it to completion regardless of the tool's own outcome): {result:?}"
        );
        assert!(
            !fixture.target_workspace.join("shell_proof.txt").exists(),
            "regression: with the target's OWN configured Docker sandbox genuinely attached \
             through execute_agentic's Bounded branch, a shell write inside its workspace \
             must fail (read-only mount, or a dialect mismatch inside the container) - today \
             the rebuilt shell tool silently gets NoopSandbox instead, so the write succeeds \
             and the file lands at {}",
            fixture.target_workspace.join("shell_proof.txt").display()
        );
    }

    /// Same shape as `bounded_delegate_fs_fixture`, but assembles the
    /// caller's `parent_tools` through the FULL production registry
    /// (`all_tools_with_runtime`) instead of the smaller
    /// `default_tools_with_runtime`, so tools outside `FILESYSTEM_TOOL_NAMES`
    /// (`git_operations`, `backup`, `data_management`, ...) are present too -
    /// exactly like a real caller turn with those features enabled.
    /// `configure` lets each test flip config flags (e.g.
    /// `data_retention.enabled`) before the registry is built.
    async fn bounded_delegate_full_fixture(
        tool_name: &str,
        configure: impl FnOnce(&mut Config),
    ) -> BoundedDelegateFsFixture {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let caller_workspace = tmp.path().join("caller-session-cwd");
        std::fs::create_dir_all(&caller_workspace).unwrap();

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "balanced".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec![tool_name.to_string(), "delegate".to_string()],
                allowed_commands: vec!["*".to_string()],
                block_high_risk_commands: false,
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "research".to_string(),
            RiskProfileConfig {
                allowed_tools: vec![tool_name.to_string()],
                allowed_commands: vec!["*".to_string()],
                block_high_risk_commands: false,
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "fs_agentic_test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 5,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "executive_assistant".to_string(),
            AliasedAgentConfig {
                risk_profile: "balanced".into(),
                runtime_profile: "fs_agentic_test".into(),
                model_provider: "ollama.executive_assistant".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "fs_researcher".to_string(),
                    mode: DelegateExecutionMode::Bounded,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        let target_config = AliasedAgentConfig {
            risk_profile: "research".into(),
            runtime_profile: "fs_agentic_test".into(),
            model_provider: "ollama.fs_researcher".into(),
            ..AliasedAgentConfig::default()
        };
        config
            .agents
            .insert("fs_researcher".to_string(), target_config.clone());

        configure(&mut config);

        let target_workspace = config.agent_workspace_dir("fs_researcher");
        assert_ne!(
            caller_workspace, target_workspace,
            "test precondition: caller session cwd must differ from the target's configured workspace"
        );

        let config = Arc::new(config);

        let mut caller_policy = SecurityPolicy::for_agent(&config, "executive_assistant")
            .expect("caller policy resolves");
        caller_policy.workspace_dir = caller_workspace.clone();
        let caller_policy = Arc::new(caller_policy);

        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(crate::platform::NativeRuntime::new());
        let memory: Arc<dyn Memory> =
            Arc::new(SqliteMemory::new("bounded-full-fixture", &config.data_dir).unwrap());
        let caller_risk_profile = config
            .risk_profiles
            .get("balanced")
            .expect("balanced profile inserted above")
            .clone();
        let browser = zeroclaw_config::schema::BrowserConfig {
            enabled: false,
            ..zeroclaw_config::schema::BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let web_fetch = zeroclaw_config::schema::WebFetchConfig::default();

        // Build via the SAME factory production uses for a caller's full
        // registry (`all_tools_with_runtime`), so this fixture cannot
        // silently drift from what a real caller turn actually assembles.
        let caller_tool_registry = crate::tools::all_tools_with_runtime(
            Arc::clone(&config),
            &caller_policy,
            &caller_risk_profile,
            "executive_assistant",
            Arc::clone(&runtime),
            memory,
            None,
            None,
            &browser,
            &http,
            &web_fetch,
            &caller_workspace,
            &config.agents,
            None,
            &config,
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .tools;

        let real_tool = caller_tool_registry
            .into_iter()
            .find(|t| t.name() == tool_name)
            .unwrap_or_else(|| panic!("all_tools_with_runtime did not register '{tool_name}'"));
        let parent_tools: Vec<Arc<dyn Tool>> = vec![Arc::from(real_tool)];

        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }

        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_workspace_dir(caller_workspace.clone())
            .with_parent_tools(Arc::new(RwLock::new(parent_tools)))
            .with_runtime(runtime)
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_caller_alias("executive_assistant");

        BoundedDelegateFsFixture {
            _tmp: tmp,
            tool,
            target_config,
            caller_workspace,
            target_workspace,
            config,
        }
    }

    #[tokio::test]
    async fn bounded_delegate_git_operations_operates_in_target_workspace_not_callers() {
        // Regression: `git_operations` is not in `FILESYSTEM_TOOL_NAMES`
        // (tools/mod.rs:358-367), so the Bounded rebuild's fallback
        // (delegate.rs:2720-2732) hands the target the CALLER's already-built
        // `GitOperationsTool` instance (git_operations.rs:11-20 bakes in
        // `workspace_dir` at construction) instead of one bound to the
        // target's own workspace.
        let fixture = bounded_delegate_full_fixture("git_operations", |_cfg| {}).await;

        // Unlike the caller's session cwd (created explicitly by the
        // fixture), a target's configured workspace is only ever created
        // lazily by real filesystem tools - `git init` needs the directory to
        // already exist.
        std::fs::create_dir_all(&fixture.target_workspace).unwrap();

        // Both workspaces get a valid repo (an uninitialized target would make
        // `git status` error out through `Tool::execute()`'s `Err` path,
        // which the agentic loop's own max-iterations safety net handles
        // very differently from a plain `ToolResult{success:false}` - a
        // distinct, real finding, but not what this test is about). Each
        // repo gets its OWN branch name, so the reported branch tells us
        // which workspace `git_operations` actually read from.
        for (dir, branch) in [
            (&fixture.caller_workspace, "caller-branch"),
            (&fixture.target_workspace, "target-branch"),
        ] {
            let status = std::process::Command::new("git")
                .args(["init", "-q", "-b", branch])
                .current_dir(dir)
                .status()
                .expect("git must be available to run this test");
            assert!(status.success(), "git init failed in {}", dir.display());
        }

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "git_operations",
            tool_args: serde_json::json!({ "operation": "status" }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "check status",
                Some(0.2),
            )
            .await
            .unwrap();

        let output = result.output.to_string();
        assert!(
            !output.contains("caller-branch"),
            "regression: a Bounded delegate's git_operations must NOT report the \
             CALLER's branch ('caller-branch') just because its tool instance was \
             reused from parent_tools - got: {output}"
        );
        assert!(
            output.contains("target-branch"),
            "regression: a Bounded delegate's git_operations must run 'git status' \
             against the TARGET's own workspace (branch 'target-branch'), not the \
             caller's - got: {output}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_pushover_reads_credentials_from_target_workspace_not_callers() {
        // Regression: `pushover` was absent from both `FILESYSTEM_TOOL_NAMES`
        // and `WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT`, so a Bounded
        // cross-profile target got the CALLER's `PushoverTool` instance
        // (pushover.rs:17-22 bakes in `workspace_dir` at construction), and
        // `get_credentials` (pushover.rs:44-58) always reads
        // `self.workspace_dir.join(".env")` for PUSHOVER_TOKEN/PUSHOVER_USER_KEY.
        //
        // Neither workspace gets a `.env` file here on purpose: `get_credentials`
        // fails and returns before any HTTP request is built, so this assertion
        // is safe (no real network call to Pushover) whether the bug is present
        // or fixed. The failure text embeds the exact `.env` path it tried to
        // read (`pushover.rs:57`), which is enough to prove which workspace's
        // credentials a bounded target would actually read from.
        let fixture = bounded_delegate_full_fixture("pushover", |_cfg| {}).await;
        std::fs::create_dir_all(&fixture.target_workspace).unwrap();

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "pushover",
            tool_args: serde_json::json!({ "message": "hello" }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "send a notification",
                Some(0.2),
            )
            .await
            .unwrap();

        let output = result.output.to_string();
        // The tool message this echoes is itself JSON-encoded (see
        // `ToolExecutionOutcome`/the turn's tool-result envelope), so a
        // Windows path's backslashes appear doubled (`\\`) inside `output`.
        let json_escape = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
        let caller_env = json_escape(&fixture.caller_workspace.join(".env"));
        let target_env = json_escape(&fixture.target_workspace.join(".env"));
        assert!(
            !output.contains(&caller_env),
            "regression: a Bounded delegate's pushover must NOT look for credentials \
             in the CALLER's .env ({caller_env}) just because its tool instance was \
             reused from parent_tools - got: {output}"
        );
        assert!(
            output.contains(&target_env),
            "regression: a Bounded delegate's pushover must look for credentials in \
             the TARGET's own workspace's .env ({target_env}), not the caller's - \
             got: {output}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_backup_creates_archive_in_target_workspace_not_callers() {
        // Regression: `backup` is not in `FILESYSTEM_TOOL_NAMES` either, so a
        // Bounded cross-profile target gets the CALLER's `BackupTool`
        // instance (tools/mod.rs:1182-1186 bakes in `workspace_dir` at
        // construction), and `cmd_create` (backup_tool.rs:30-62) always
        // archives into `self.workspace_dir.join("backups")`.
        let fixture = bounded_delegate_full_fixture("backup", |_cfg| {}).await;

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "backup",
            tool_args: serde_json::json!({ "command": "create" }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "create a backup",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(result.success, "bounded backup delegate failed: {result:?}");

        assert!(
            !fixture.caller_workspace.join("backups").exists(),
            "regression: a Bounded delegate's backup must NOT create its archive \
             under the caller's session workspace ({}) just because its tool \
             instance was reused from parent_tools",
            fixture.caller_workspace.display()
        );
        assert!(
            fixture.target_workspace.join("backups").exists(),
            "regression: a Bounded delegate's backup must create its archive \
             under the TARGET's own configured workspace ({}), not the caller's",
            fixture.target_workspace.display()
        );
    }

    #[tokio::test]
    async fn bounded_delegate_data_management_purge_does_not_delete_callers_files() {
        // Regression (most severe variant of the bounded cross-profile workspace
        // boundary bug): `data_management` is not
        // in `FILESYSTEM_TOOL_NAMES` either, and unlike file_write/backup this
        // tool is DESTRUCTIVE - `cmd_purge` (data_management.rs:41-59) calls
        // `fs::remove_file` (data_management.rs:205) against
        // `self.workspace_dir`. A Bounded cross-profile target reusing the
        // caller's instance can delete files from the CALLER's workspace.
        let fixture = bounded_delegate_full_fixture("data_management", |cfg| {
            cfg.data_retention.enabled = true;
            cfg.data_retention.retention_days = 0;
        })
        .await;

        let canary = fixture.caller_workspace.join("do_not_delete.txt");
        tokio::fs::write(&canary, b"caller data").await.unwrap();
        // Guarantee the canary's mtime second is strictly before the purge
        // cutoff (`retention_days = 0` makes the cutoff "now", truncated to
        // whole seconds) instead of racing the clock.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "data_management",
            tool_args: serde_json::json!({ "command": "purge", "dry_run": false }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "purge old data",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "bounded data_management delegate failed: {result:?}"
        );

        assert!(
            canary.exists(),
            "CRITICAL regression: a Bounded delegate's data_management purge must \
             NEVER delete files from the caller's workspace ({}) just because its \
             tool instance was reused from parent_tools - this is a destructive \
             variant of #9872, not just a read/write leak",
            fixture.caller_workspace.display()
        );
    }

    // ── IDENTITY_BOUND_TOOL_NAMES regressions ────────────────────────────
    //
    // Same root cause as every test above (the `Bounded` branch's fallback
    // reuses the caller's already-built `ToolArcRef`-wrapped instance), but a
    // different capture mechanism: these tools bind the CALLER's
    // `agent_alias` at construction, not `workspace_dir`/`SecurityPolicy`.
    // Found by a full audit of `all_tools_with_runtime` prompted by the
    // question "why does every review round keep finding one more tool" -
    // see the investigation notes for the complete reasoning.

    #[tokio::test]
    async fn bounded_delegate_read_skill_reads_target_workspace_skill_not_callers() {
        // Regression: `read_skill` captures the CALLER's `agent_alias` at
        // construction (read_skill.rs:9-19), and `execute()` resolves skills
        // via `load_skills_for_agent_from_config(&self.config, &self.agent_alias)`
        // (read_skill.rs:67), which resolves workspace skills through
        // `config.agent_workspace_dir(agent_alias)` (confirmed by the dedicated
        // test `load_skills_for_agent_from_config_uses_workspace_dir_not_data_dir`
        // in skills/mod.rs). A Bounded cross-profile target reusing the
        // caller's instance would read the CALLER's workspace skills.
        let fixture = bounded_delegate_full_fixture("read_skill", |cfg| {
            cfg.skills.prompt_injection_mode =
                zeroclaw_config::schema::SkillsPromptInjectionMode::Compact;
        })
        .await;
        std::fs::create_dir_all(&fixture.target_workspace).unwrap();

        for (dir, marker) in [
            (&fixture.caller_workspace, "CALLER_SKILL_MARKER"),
            (&fixture.target_workspace, "TARGET_SKILL_MARKER"),
        ] {
            let skill_dir = dir.join("skills").join("probe");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.toml"),
                format!(
                    "[skill]\nname = \"probe\"\ndescription = \"{marker}\"\nversion = \"0.1.0\"\n"
                ),
            )
            .unwrap();
        }

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "read_skill",
            tool_args: serde_json::json!({ "name": "probe" }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "read the probe skill",
                Some(0.2),
            )
            .await
            .unwrap();

        let output = result.output.to_string();
        assert!(
            !output.contains("CALLER_SKILL_MARKER"),
            "regression: a Bounded delegate's read_skill must NOT return the CALLER's \
             skill content just because its tool instance was reused from \
             parent_tools - got: {output}"
        );
        assert!(
            output.contains("TARGET_SKILL_MARKER"),
            "regression: a Bounded delegate's read_skill must return the TARGET's own \
             workspace skill, not the caller's - got: {output}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_cron_add_stores_job_owned_by_target_not_caller() {
        // Regression: `cron_add` captures the CALLER's `agent_alias` at
        // construction (cron_add.rs:14-21, doc: "Cron jobs created here are
        // validated against this agent's risk profile and run as this
        // agent"). The scheduler later re-derives `SecurityPolicy::for_agent`
        // from the job's STORED `agent_alias` (cron/scheduler.rs:558,699) and
        // runs the job under that identity's full risk profile - so a
        // Bounded cross-profile target reusing the caller's instance could
        // plant a persistent job that later runs, autonomously, with the
        // CALLER's permissions. This test only checks which identity the
        // created job is stored under - it never lets the job actually run
        // (no scheduler tick fires here), which is enough to prove the fix
        // without any of the async-execution risk that would come with
        // actually running it.
        let fixture = bounded_delegate_full_fixture("cron_add", |_cfg| {}).await;

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "cron_add",
            tool_args: serde_json::json!({
                "job_type": "shell",
                "command": "echo hi",
                "schedule": { "kind": "every", "every_ms": 3_600_000 },
                "approved": true,
            }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "add a recurring job",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "bounded cron_add delegate failed: {result:?}"
        );

        let caller_jobs =
            crate::cron::list_jobs_by_agent(&fixture.config, "executive_assistant").unwrap();
        assert!(
            caller_jobs.is_empty(),
            "regression: a Bounded delegate's cron_add must NOT store the created job \
             under the CALLER's agent_alias just because its tool instance was reused \
             from parent_tools - got: {caller_jobs:?}"
        );
        let target_jobs =
            crate::cron::list_jobs_by_agent(&fixture.config, "fs_researcher").unwrap();
        assert_eq!(
            target_jobs.len(),
            1,
            "regression: a Bounded delegate's cron_add must store the created job under \
             the TARGET's own agent_alias, not the caller's"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_cron_update_resolves_target_jobs_by_name_not_callers() {
        // Regression: `cron_update` captures the CALLER's `agent_alias`
        // (cron_update.rs:13-19) and resolves a job's `job_id` argument via
        // `cron::resolve_job_id_or_name(&self.config, raw_id, &self.agent_alias)`
        // (cron_update.rs:239), which - for name-based lookup - is scoped to
        // that alias's own jobs (`resolve_job_id_or_name`,
        // cron/store.rs:258-283: falls back to `list_jobs_by_agent(config,
        // agent_alias)` when the raw string isn't an existing job ID). A
        // Bounded cross-profile target reusing the caller's instance could
        // not resolve (or worse, collide with) its own jobs by name.
        let fixture = bounded_delegate_full_fixture("cron_update", |_cfg| {}).await;

        crate::cron::add_shell_job(
            &fixture.config,
            "fs_researcher",
            Some("probe".to_string()),
            crate::cron::Schedule::Every {
                every_ms: 3_600_000,
            },
            "echo hi",
        )
        .unwrap();

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "cron_update",
            tool_args: serde_json::json!({
                "job_id": "probe",
                "patch": { "name": "renamed-probe" }
            }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "rename the probe job",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "regression: a Bounded delegate's cron_update must resolve job names \
             against the TARGET's own jobs, not the caller's - got: {result:?}"
        );
        let target_jobs =
            crate::cron::list_jobs_by_agent(&fixture.config, "fs_researcher").unwrap();
        assert!(
            target_jobs
                .iter()
                .any(|j| j.name.as_deref() == Some("renamed-probe")),
            "cron_update did not actually rename the target's job - got: {target_jobs:?}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_cron_remove_resolves_target_jobs_by_name_not_callers() {
        // Regression: `cron_remove` captures the CALLER's `agent_alias`
        // (cron_remove.rs:9-14, doc: "scopes name resolution to this agent's
        // own jobs") - same `resolve_job_id_or_name` mechanism as
        // `cron_update` above. A Bounded cross-profile target reusing the
        // caller's instance could not remove its own job by name.
        let fixture = bounded_delegate_full_fixture("cron_remove", |_cfg| {}).await;

        crate::cron::add_shell_job(
            &fixture.config,
            "fs_researcher",
            Some("probe".to_string()),
            crate::cron::Schedule::Every {
                every_ms: 3_600_000,
            },
            "echo hi",
        )
        .unwrap();

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "cron_remove",
            tool_args: serde_json::json!({ "job_id": "probe" }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "remove the probe job",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "regression: a Bounded delegate's cron_remove must resolve job names \
             against the TARGET's own jobs, not the caller's - got: {result:?}"
        );
        let target_jobs =
            crate::cron::list_jobs_by_agent(&fixture.config, "fs_researcher").unwrap();
        assert!(
            target_jobs.is_empty(),
            "cron_remove did not actually delete the target's job - got: {target_jobs:?}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_schedule_stores_job_owned_by_target_not_caller() {
        // Regression: `schedule` captures the CALLER's `agent_alias`
        // (schedule.rs:13-19, doc: "risk profile gate for shell command
        // validation") and creates jobs through the SAME `cron::` store
        // functions as `cron_add` (schedule.rs:409-413), so it shares that
        // tool's "runs later under the stored identity's risk profile"
        // exposure. Same safe assertion strategy as `cron_add`: only checks
        // which identity the job is stored under, never lets it run.
        let fixture = bounded_delegate_full_fixture("schedule", |_cfg| {}).await;

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "schedule",
            tool_args: serde_json::json!({
                "action": "create",
                "expression": "0 9 * * 1-5",
                "command": "echo hi",
                "approved": true,
            }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "create a recurring job",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "bounded schedule delegate failed: {result:?}"
        );

        let caller_jobs =
            crate::cron::list_jobs_by_agent(&fixture.config, "executive_assistant").unwrap();
        assert!(
            caller_jobs.is_empty(),
            "regression: a Bounded delegate's schedule must NOT store the created job \
             under the CALLER's agent_alias just because its tool instance was reused \
             from parent_tools - got: {caller_jobs:?}"
        );
        let target_jobs =
            crate::cron::list_jobs_by_agent(&fixture.config, "fs_researcher").unwrap();
        assert_eq!(
            target_jobs.len(),
            1,
            "regression: a Bounded delegate's schedule must store the created job under \
             the TARGET's own agent_alias, not the caller's"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_send_message_to_peer_uses_target_alias_not_callers() {
        // Regression: `send_message_to_peer` captures the CALLER's
        // `sender_alias` at construction (send_message_to_peer.rs:18-25, doc:
        // "Bound to a single calling agent's alias; the tool validates every
        // send against that agent's resolved peer set") and both of its
        // rejection paths embed that alias in the error text
        // (send_message_to_peer.rs:142-146,162-166), reached BEFORE any real
        // channel delivery is attempted - so this is safe to drive through
        // the real `execute_agentic` path without a peer group configured at
        // all (the first rejection fires immediately). A Bounded
        // cross-profile target reusing the caller's instance would have its
        // sends validated against the CALLER's peer set/channels, not its
        // own.
        let fixture = bounded_delegate_full_fixture("send_message_to_peer", |_cfg| {}).await;

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "send_message_to_peer",
            tool_args: serde_json::json!({
                "channel": "telegram.test",
                "target": "someone",
                "message": "hi",
            }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "send a message",
                Some(0.2),
            )
            .await
            .unwrap();

        let output = result.output.to_string();
        assert!(
            !output.contains("executive_assistant"),
            "regression: a Bounded delegate's send_message_to_peer must NOT resolve \
             the peer set/channel membership using the CALLER's alias - got: {output}"
        );
        assert!(
            output.contains("fs_researcher"),
            "regression: a Bounded delegate's send_message_to_peer must resolve using \
             the TARGET's own alias, not the caller's - got: {output}"
        );
    }

    #[tokio::test]
    async fn spawn_subagent_tool_factory_binds_given_alias_for_risk_profile_admission() {
        // Wiring-level test, NOT through execute_agentic, and NOT one of the
        // `bounded_delegate_*` regressions above. `SpawnSubagentTool::execute()`
        // ends by calling `crate::agent::run(...)` (spawn_subagent.rs:223-236)
        // - a full recursive agent turn that resolves its OWN model provider
        // from config internally, bypassing this test suite's
        // `execute_agentic`-level model-provider injection entirely. Unlike
        // `ClaudeCodeTool` (`new_with_executor` supports injecting a fake
        // executor - the technique used for the `claude_code` regression
        // elsewhere in this file), `SpawnSubagentTool` has no injection point
        // to fake that call, and delegate-level admission
        // (`self.security.is_tool_allowed`/`Self::delegate_admits_with_mcp`)
        // necessarily requires BOTH the caller's and the target's risk
        // profile to already admit "spawn_subagent" before a Bounded call
        // even reaches this tool's `execute()` - so by the time execution
        // gets here, the tool's OWN internal admission check
        // (`config.risk_profile_for_agent(&self.parent_alias)`,
        // spawn_subagent.rs:104-119) is, in the fixed code, checking the same
        // permission the delegate layer already confirmed, and would pass
        // too - meaning there is no way to observe a SAFE, discriminating
        // rejection from inside a real Bounded turn without either
        // completing the real recursive run (unverified whether that is
        // network-free - not worth the risk to find out) or losing the
        // signal. This test instead proves the narrower, real thing that
        // actually differs between the bug and the fix: `delegate.rs`'s
        // `Bounded` branch calls `crate::tools::spawn_subagent_tool(...,
        // agent_name, ...)` - this test calls the SAME factory directly with
        // a target alias whose OWN risk profile excludes "spawn_subagent",
        // and confirms the constructed tool's admission check is genuinely
        // keyed off the alias it was given (not hardcoded, not ignored, not
        // silently defaulted to some other identity) - the exact fact the
        // production fix depends on.
        use zeroclaw_config::schema::{AliasedAgentConfig, RiskProfileConfig};

        let mut config = Config {
            config_path: std::path::PathBuf::from("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "excludes_spawn".to_string(),
            RiskProfileConfig {
                excluded_tools: vec!["spawn_subagent".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.agents.insert(
            "restricted_target".to_string(),
            AliasedAgentConfig {
                risk_profile: "excludes_spawn".into(),
                ..AliasedAgentConfig::default()
            },
        );

        let tool = crate::tools::spawn_subagent_tool(
            Arc::new(config),
            "restricted_target",
            Arc::new(SecurityPolicy::default()),
            false,
            None,
        );

        let result = tool
            .execute(serde_json::json!({ "prompt": "hello" }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "expected the target alias's own excluded_tools to block spawn_subagent - \
             got: {result:?}"
        );
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("restricted_target"),
            "the rejection must name the alias that was actually checked - proving \
             crate::tools::spawn_subagent_tool (the same factory delegate.rs's Bounded \
             branch calls) binds the given alias rather than ignoring it - got: {result:?}"
        );
    }

    /// Test double for `zeroclaw_tools::coding_cli::CodingCliExecutor` used ONLY by
    /// `bounded_delegate_claude_code_...` below. `claude_code` (like the other
    /// coding-CLI tools) shells out to a real external binary via this trait -
    /// unlike `git_operations`/`backup`/`data_management` above, a real executor
    /// (`DirectCodingCliExecutor`/`RuntimeCodingCliExecutor`) would risk actually
    /// invoking a real `claude`/`codex`/`gemini`/`opencode` process once the
    /// containment check below passes. This fake just records the working
    /// directory it was asked to run in and returns a canned success - it never
    /// spawns anything, so this test stays safe both BEFORE and AFTER the
    /// Bloqueante 2 fix lands (before: the containment check rejects the call and
    /// this executor is never even reached; after: it's reached, but is inert).
    #[derive(Default)]
    struct FakeCodingCliExecutor {
        received_working_dir: std::sync::Mutex<Option<PathBuf>>,
    }

    #[async_trait]
    impl zeroclaw_tools::coding_cli::CodingCliExecutor for FakeCodingCliExecutor {
        async fn output(
            &self,
            command: zeroclaw_tools::coding_cli::CodingCliCommand,
        ) -> Result<std::process::Output, zeroclaw_tools::coding_cli::CodingCliExecutionError>
        {
            *self.received_working_dir.lock().unwrap() = Some(command.working_dir);
            Ok(fake_success_process_output())
        }
    }

    fn fake_success_process_output() -> std::process::Output {
        std::process::Output {
            status: fake_success_exit_status(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    #[cfg(windows)]
    fn fake_success_exit_status() -> std::process::ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(0)
    }

    #[cfg(unix)]
    fn fake_success_exit_status() -> std::process::ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(0)
    }

    /// Real regression test for the coding-CLI class of the Bloqueante 2 gap,
    /// exercised through the real `DelegateTool::execute_agentic` Bounded branch -
    /// but with `parent_tools` built by hand (the ONE deliberate exception to
    /// "always build via the real factory" in this file) so a `FakeCodingCliExecutor`
    /// can be injected instead of the real one `all_tools_with_runtime` would wire in.
    ///
    /// SAFETY-CRITICAL DESIGN NOTE - do not "simplify" this back to asserting a
    /// TARGET-valid `working_directory` gets ACCEPTED: an earlier version of this
    /// test did exactly that, and once the fix correctly rebuilds `claude_code`
    /// against `target_policy`, containment passes and execution proceeds past the
    /// injected fake straight into a BRAND NEW, REAL `RuntimeCodingCliExecutor`
    /// (the reconstruction never reuses the caller's original executor, fake or
    /// not - see `crate::tools::claude_code_tool`). That earlier version very
    /// likely spawned the real `claude` CLI installed on the dev machine via
    /// `which::which("claude")` (`zeroclaw-tools/src/coding_cli.rs`). This version
    /// instead asserts the OPPOSITE direction: a `working_directory` valid only
    /// under the CALLER's own workspace must be REJECTED. That is safe in BOTH
    /// possible code states - if this regression were ever reintroduced, the
    /// reused caller instance (wrapping THIS test's fake executor) would accept
    /// the path and hit the fake, not a real process; with the fix in place, the
    /// path is rejected before any executor - real or fake - is ever reached. The
    /// assertion on `fake_executor.received_working_dir` staying `None` is the
    /// direct proof no executor path was reached either way.
    #[tokio::test]
    async fn bounded_delegate_claude_code_working_directory_is_scoped_to_target_workspace_not_callers()
     {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let caller_workspace = tmp.path().join("caller-session-cwd");
        std::fs::create_dir_all(&caller_workspace).unwrap();

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        // Production only ever puts `claude_code` in a caller's `parent_tools` when
        // this is enabled (`all_tools_with_runtime` gates it) - the reconstruction
        // path being tested gates on the SAME flag, so leaving it unset here would
        // make `claude_code_tool()` return `None` and silently fall through to the
        // pre-fix `ToolArcRef` fallback, which is not what this test means to probe.
        config.claude_code.enabled = true;
        config.risk_profiles.insert(
            "balanced".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec!["claude_code".to_string(), "delegate".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "research".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["claude_code".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "fs_agentic_test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 5,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "executive_assistant".to_string(),
            AliasedAgentConfig {
                risk_profile: "balanced".into(),
                runtime_profile: "fs_agentic_test".into(),
                model_provider: "ollama.executive_assistant".into(),
                delegates: vec![DelegateTargetConfig {
                    agent: "fs_researcher".to_string(),
                    mode: DelegateExecutionMode::Bounded,
                }],
                ..AliasedAgentConfig::default()
            },
        );
        let target_config = AliasedAgentConfig {
            risk_profile: "research".into(),
            runtime_profile: "fs_agentic_test".into(),
            model_provider: "ollama.fs_researcher".into(),
            ..AliasedAgentConfig::default()
        };
        config
            .agents
            .insert("fs_researcher".to_string(), target_config.clone());

        let target_workspace = config.agent_workspace_dir("fs_researcher");
        assert_ne!(
            caller_workspace, target_workspace,
            "test precondition: caller session cwd must differ from the target's configured workspace"
        );

        let config = Arc::new(config);
        let mut caller_policy = SecurityPolicy::for_agent(&config, "executive_assistant")
            .expect("caller policy resolves");
        caller_policy.workspace_dir = caller_workspace.clone();
        let caller_policy = Arc::new(caller_policy);

        let fake_executor = Arc::new(FakeCodingCliExecutor::default());
        let claude_code_tool: Arc<dyn Tool> = Arc::new(crate::tools::RateLimitedTool::new(
            crate::tools::ClaudeCodeTool::new_with_executor(
                Arc::clone(&caller_policy),
                zeroclaw_config::schema::ClaudeCodeConfig::default(),
                Arc::clone(&fake_executor)
                    as Arc<dyn zeroclaw_tools::coding_cli::CodingCliExecutor>,
            ),
            Arc::clone(&caller_policy),
        ));

        let mut delegate_agents = HashMap::new();
        for (name, agent) in &config.agents {
            delegate_agents.insert(name.clone(), agent.clone());
        }

        let tool = DelegateTool::new(delegate_agents, None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_workspace_dir(caller_workspace.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![claude_code_tool])))
            .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_caller_alias("executive_assistant");

        std::fs::create_dir_all(&target_workspace).unwrap();
        // Deliberately create this ONLY under the caller's workspace, not the
        // target's - see the safety note on this test above. A path valid under
        // the caller's workspace must be rejected once `claude_code` is correctly
        // rebound to the target's own policy.
        let caller_only_subdir = caller_workspace.join("caller_only");
        std::fs::create_dir_all(&caller_only_subdir).unwrap();

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "claude_code",
            tool_args: serde_json::json!({
                "prompt": "irrelevant - execution must never reach any executor, real or fake",
                "working_directory": caller_only_subdir.to_string_lossy(),
            }),
        };

        let result = tool
            .execute_agentic(
                "fs_researcher",
                &target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "run a coding task",
                Some(0.2),
            )
            .await
            .unwrap();

        let output = result.output.to_string();
        assert!(
            output.contains("is outside the workspace"),
            "regression: a Bounded delegate's claude_code must validate `working_directory` \
             against the TARGET's own configured workspace, not the caller's - a directory \
             that only exists under the caller's workspace must be REJECTED, but got: {output}"
        );
        assert_eq!(
            *fake_executor.received_working_dir.lock().unwrap(),
            None,
            "the CLI executor must NEVER be reached when working_directory is outside the \
             target's own workspace - if this fails, execution reached the executor stage \
             (a REAL RuntimeCodingCliExecutor in the fixed code path, not this fake)"
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
        // The bounded sealing pass must not synthesize a PipelineTool from
        // caller config when the parent registry did not contain one.
        config.pipeline.enabled = true;
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
    async fn bounded_agentic_tools_drop_deliver_file_without_acp_transport() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        struct DeliverFileFixture;

        impl zeroclaw_api::attribution::Attributable for DeliverFileFixture {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
            }

            fn alias(&self) -> &str {
                "deliver_file"
            }
        }

        #[async_trait]
        impl Tool for DeliverFileFixture {
            fn name(&self) -> &str {
                "deliver_file"
            }

            fn description(&self) -> &str {
                "Test-only ACP delivery capability"
            }

            fn parameters_schema(&self) -> serde_json::Value {
                json!({"type": "object"})
            }

            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                unreachable!("the fixture must be removed before child execution")
            }
        }

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
                allowed_tools: vec!["deliver_file".to_string(), DelegateTool::NAME.to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["deliver_file".to_string()],
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
        assert!(caller_policy.is_tool_allowed("deliver_file"));

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(DeliverFileFixture)])));
        let target_config = config
            .agents
            .get("target")
            .expect("target agent exists")
            .clone();
        let target_tool_policy = tool
            .resolve_tool_policy(&target_config.risk_profile)
            .expect("target tool policy resolves");
        assert!(DelegateTool::delegate_admits_with_mcp(
            &target_tool_policy,
            "deliver_file"
        ));

        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &ToolCountModelProvider { expected_tools: 0 },
                "deliver a file",
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

    #[cfg(unix)]
    #[tokio::test]
    async fn independent_delegate_denies_prompt_required_skill_tools_without_approval_route() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let target_ws = tmp.path().join("target-workspace");
        let marker = target_ws.join("independent-delegate-marker");
        std::fs::create_dir_all(target_ws.join("skills/rm_marker")).unwrap();
        std::fs::write(&marker, b"must survive").unwrap();
        std::fs::write(
            target_ws.join("skills/rm_marker/SKILL.toml"),
            r#"[skill]
name = "rm_marker"
description = "test skill for independent approval policy"
version = "0.1.0"

[[tools]]
name = "remove"
description = "remove the marker"
kind = "shell"
command = "rm independent-delegate-marker"
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
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Supervised,
                allowed_commands: vec!["rm".to_string()],
                allowed_tools: vec!["shell".to_string()],
                block_high_risk_commands: true,
                require_approval_for_medium_risk: true,
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 2,
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
        let delegate = DelegateTool::new(config.agents.clone(), None, caller_policy)
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(NativeRuntime::new()))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));
        let target = config.agents.get("target").unwrap();
        let provider = IndependentRiskPolicyModelProvider::default();

        let result = delegate
            .execute_agentic(
                "target",
                target,
                "test",
                "test-model",
                &provider,
                "remove the marker",
                None,
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "target should complete after denied calls: {result:?}"
        );
        assert!(
            marker.exists(),
            "a prompt-required skill command must not dispatch without an approval route"
        );
        let tool_messages = provider.tool_messages();
        assert!(
            tool_messages.iter().any(|message| {
                message.contains("requires approval and no operator decision was available")
            }),
            "nested tool result must report runtime fail-closed denial: {tool_messages:?}"
        );
        assert!(
            tool_messages.iter().any(|message| {
                message.contains("Command requires explicit approval (approved=true)")
            }),
            "built-in shell must still receive approved=false and enforce command policy: {tool_messages:?}"
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

    fn fallback_delegate_config(
        primary_uri: String,
        backup_uri: String,
        agentic: bool,
    ) -> (Arc<Config>, TempDir) {
        fallback_delegate_config_with_native_tools(primary_uri, backup_uri, agentic, None, None)
    }

    fn fallback_delegate_config_with_native_tools(
        primary_uri: String,
        backup_uri: String,
        agentic: bool,
        primary_native_tools: Option<bool>,
        backup_native_tools: Option<bool>,
    ) -> (Arc<Config>, TempDir) {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, CustomModelProviderConfig, McpBundleConfig,
            McpServerConfig, ModelProviderConfig, RiskProfileConfig, RuntimeProfileConfig,
        };

        let temp_dir = TempDir::new().expect("temporary delegate fixture directory");
        let mut config = Config {
            data_dir: temp_dir.path().join("data"),
            config_path: temp_dir.path().join("config.toml"),
            ..Config::default()
        };
        config.reliability.provider_retries = 0;
        config.reliability.provider_backoff_ms = 1;
        config.providers.models.custom.insert(
            "primary".to_string(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(primary_uri),
                    model: Some("primary-model".to_string()),
                    native_tools: primary_native_tools,
                    fallback: vec![zeroclaw_config::providers::ModelProviderRef::new(
                        "custom.backup",
                    )],
                    ..ModelProviderConfig::default()
                },
            },
        );
        config.providers.models.custom.insert(
            "backup".to_string(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(backup_uri),
                    // This deliberately matches the primary model. The two aliases
                    // still represent distinct configured candidates.
                    model: Some("primary-model".to_string()),
                    native_tools: backup_native_tools,
                    ..ModelProviderConfig::default()
                },
            },
        );
        config.risk_profiles.insert(
            "delegating".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "review".to_string(),
            RuntimeProfileConfig {
                agentic,
                ..RuntimeProfileConfig::default()
            },
        );
        // Both agents are granted the `echo_srv` MCP server, so `McpEchoTool`
        // reaches a bounded target through the target's OWN bundle grant rather
        // than by inheriting the caller's instance.
        config.mcp.servers = vec![McpServerConfig {
            name: "echo_srv".to_string(),
            ..McpServerConfig::default()
        }];
        config.mcp_bundles.insert(
            "echo_bundle".to_string(),
            McpBundleConfig {
                servers: vec!["echo_srv".to_string()],
                exclude: Vec::new(),
            },
        );
        for agent in ["caller", "target"] {
            config.agents.insert(
                agent.to_string(),
                AliasedAgentConfig {
                    model_provider: "custom.primary".into(),
                    risk_profile: "delegating".into(),
                    runtime_profile: "review".into(),
                    mcp_bundles: vec!["echo_bundle".to_string()],
                    ..AliasedAgentConfig::default()
                },
            );
        }

        (Arc::new(config), temp_dir)
    }

    fn fallback_delegate_tool(config: Arc<Config>, workspace_dir: Option<PathBuf>) -> DelegateTool {
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, caller_policy)
            .with_root_config(config.clone())
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone());

        match workspace_dir {
            Some(workspace_dir) => tool.with_workspace_dir(workspace_dir),
            None => tool,
        }
    }

    fn assert_generic_fallback_warning(output: &str, primary_uri: &str) {
        let warning = crate::i18n::get_required_cli_string("delegate-provider-fallback-warning");
        assert_eq!(
            output.matches(&warning).count(),
            1,
            "recovered delegate result must include exactly one generic warning: {output:?}"
        );
        assert!(
            !output.contains(primary_uri),
            "recovered output must not expose the primary endpoint: {output:?}"
        );
        assert!(
            !output.contains("synthetic primary failure"),
            "recovered output must not expose provider error details: {output:?}"
        );
    }

    fn assert_explicit_fallback_attribution(output: &str, agentic: bool) {
        let header_key = if agentic {
            "delegate-provider-fallback-header-agentic"
        } else {
            "delegate-provider-fallback-header"
        };
        let header = crate::i18n::get_required_cli_string_with_args(
            header_key,
            &[
                ("agent", "target"),
                ("requested_provider", "custom.primary"),
                ("requested_model", "primary-model"),
                ("actual_provider", "custom.backup"),
                // The same model proves this is exact candidate attribution, not a model switch.
                ("actual_model", "primary-model"),
            ],
        );
        assert_eq!(
            output.matches(&header).count(),
            1,
            "recovered output must identify the requested and served candidates exactly once: \
             {output:?}"
        );
    }

    #[tokio::test]
    async fn delegate_fallback_warning_is_local_to_synchronous_call() {
        let (primary, primary_requests) = start_failing_chat_server(503).await;
        let backup = start_final_chat_server(vec!["fallback reply"]).await;
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), true);
        let tool = fallback_delegate_tool(config, None);

        let (result, outer_fallback) =
            zeroclaw_providers::reliable::scope_provider_fallback(async {
                let result = tool
                    .execute(json!({"agent": "target", "prompt": "respond"}))
                    .await
                    .expect("delegate call completes");
                let outer_fallback = zeroclaw_providers::reliable::take_last_provider_fallback();
                (result, outer_fallback)
            })
            .await;

        assert!(result.success, "fallback should recover: {result:?}");
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must be attempted exactly once"
        );
        assert!(result.output.contains("fallback reply"), "{result:?}");
        assert_generic_fallback_warning(result.output.as_str(), &primary.uri);
        assert_explicit_fallback_attribution(result.output.as_str(), true);
        assert!(
            outer_fallback.is_none(),
            "delegate fallback must not leak into the parent channel scope: {outer_fallback:?}"
        );
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn agentic_delegate_attributes_final_primary_response_after_earlier_fallback() {
        let (primary, primary_requests) = start_primary_failure_then_final_chat_server().await;
        let (backup, backup_requests) = start_tool_call_chat_server().await;
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), true);
        let tool =
            fallback_delegate_tool(config, None).with_parent_tools(Arc::new(RwLock::new(vec![
                mcp_fixture_tool("echo_srv", "echo_tool"),
            ])));

        let result = tool
            .execute(json!({"agent": "target", "prompt": "use the echo tool, then answer"}))
            .await
            .expect("agentic delegate completes");

        assert!(result.success, "agentic delegate failed: {result:?}");
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the primary must be retried on the post-tool model request"
        );
        assert_eq!(
            backup_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the backup must serve only the tool-call response"
        );
        assert!(result.output.contains("final primary reply"), "{result:?}");
        let warning = crate::i18n::get_required_cli_string("delegate-provider-fallback-warning");
        assert!(
            !result.output.contains(&warning),
            "a final primary response must not carry stale fallback attribution: {result:?}"
        );
        assert!(
            !result.output.contains("custom.backup"),
            "the final primary response must not be labeled as backup-served: {result:?}"
        );
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn agentic_delegate_uses_text_tools_when_only_its_fallback_supports_them() {
        // The primary advertises native tools but fails. The text-only fallback
        // must receive the XML protocol and execute its tool, rather than a
        // native request that it cannot reliably interpret.
        let (primary, primary_requests) = start_failing_chat_server(503).await;
        let (backup, backup_requests, backup_bodies) =
            start_text_tool_then_final_chat_server().await;
        let (config, _fixture_dir) = fallback_delegate_config_with_native_tools(
            primary.uri.clone(),
            backup.uri.clone(),
            true,
            Some(true),
            Some(false),
        );
        let tool =
            fallback_delegate_tool(config, None).with_parent_tools(Arc::new(RwLock::new(vec![
                mcp_fixture_tool("echo_srv", "echo_tool"),
            ])));

        let result = tool
            .execute(json!({"agent": "target", "prompt": "use the echo tool, then answer"}))
            .await
            .expect("agentic delegate completes");

        assert!(result.success, "agentic delegate failed: {result:?}");
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must receive the initial failing request"
        );
        assert_eq!(
            backup_requests.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the fallback must receive the tool request and the final response request"
        );
        assert!(result.output.contains("fallback final reply"), "{result:?}");

        let bodies = backup_bodies.lock().unwrap();
        let first_request = http_request_json(bodies.first().expect("first fallback request"));
        assert!(
            first_request.get("tools").is_none(),
            "the text-only fallback must not receive native tool specifications: {first_request}"
        );
        let system_prompt = first_request["messages"]
            .as_array()
            .and_then(|messages| messages.iter().find(|message| message["role"] == "system"))
            .and_then(|message| message["content"].as_str())
            .expect("fallback request contains a text system prompt");
        for required in ["## Tool Use Protocol", "<tool_call>", "echo_tool", "value"] {
            assert!(
                system_prompt.contains(required),
                "fallback prompt must contain {required:?}: {system_prompt}"
            );
        }
        assert!(
            bodies
                .get(1)
                .is_some_and(|body| body.windows(13).any(|part| part == b"echo:fallback")),
            "the second fallback request must contain the executed tool result: {bodies:?}"
        );
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn routed_agentic_delegate_uses_selected_models_text_tool_protocol() {
        let (default, default_requests) = start_failing_chat_server(503).await;
        let (routed, routed_requests, routed_bodies) =
            start_text_tool_then_final_chat_server().await;
        let (fixture_config, _fixture_dir) = fallback_delegate_config_with_native_tools(
            default.uri.clone(),
            routed.uri.clone(),
            true,
            Some(true),
            Some(false),
        );
        let mut config = (*fixture_config).clone();
        let primary = &mut config
            .providers
            .models
            .custom
            .get_mut("primary")
            .expect("primary provider")
            .base;
        primary.model = Some("hint:text".to_string());
        primary.fallback.clear();
        config
            .providers
            .models
            .custom
            .get_mut("backup")
            .expect("routed provider")
            .base
            .model = Some("routed-model".to_string());
        config.model_routes.push(ModelRouteConfig {
            hint: "text".to_string(),
            model_provider: "custom.backup".to_string(),
            model: "routed-model".to_string(),
            api_key: None,
        });
        let tool = fallback_delegate_tool(Arc::new(config), None).with_parent_tools(Arc::new(
            RwLock::new(vec![mcp_fixture_tool("echo_srv", "echo_tool")]),
        ));

        let result = tool
            .execute(json!({"agent": "target", "prompt": "use the echo tool, then answer"}))
            .await
            .expect("routed agentic delegate completes");

        assert!(result.success, "routed delegate failed: {result:?}");
        assert_eq!(
            default_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the Router default must not receive a hinted request"
        );
        assert_eq!(
            routed_requests.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the selected route must receive both tool-loop requests"
        );
        assert!(result.output.contains("fallback final reply"), "{result:?}");

        let bodies = routed_bodies.lock().unwrap();
        let first_request = http_request_json(bodies.first().expect("first routed request"));
        assert_eq!(first_request["model"], "routed-model");
        assert!(
            first_request.get("tools").is_none(),
            "the text-only selected route must not receive native tools: {first_request}"
        );
        let system_prompt = first_request["messages"]
            .as_array()
            .and_then(|messages| messages.iter().find(|message| message["role"] == "system"))
            .and_then(|message| message["content"].as_str())
            .expect("routed request contains a text system prompt");
        for required in ["## Tool Use Protocol", "<tool_call>", "echo_tool", "value"] {
            assert!(
                system_prompt.contains(required),
                "routed prompt must contain {required:?}: {system_prompt}"
            );
        }
        assert!(
            bodies
                .get(1)
                .is_some_and(|body| body.windows(13).any(|part| part == b"echo:fallback")),
            "the second routed request must contain the tool result: {bodies:?}"
        );
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn strict_mixed_agentic_delegate_fails_before_provider_dispatch() {
        let (primary, primary_requests) = start_failing_chat_server(503).await;
        let (backup, backup_requests) = start_failing_chat_server(503).await;
        let (fixture_config, _fixture_dir) = fallback_delegate_config_with_native_tools(
            primary.uri.clone(),
            backup.uri.clone(),
            true,
            Some(true),
            Some(false),
        );
        let mut config = (*fixture_config).clone();
        config
            .runtime_profiles
            .get_mut("review")
            .expect("review runtime profile")
            .strict_tool_parsing = true;
        let tool = fallback_delegate_tool(Arc::new(config), None).with_parent_tools(Arc::new(
            RwLock::new(vec![mcp_fixture_tool("echo_srv", "echo_tool")]),
        ));

        let result = tool
            .execute(json!({"agent": "target", "prompt": "use the echo tool"}))
            .await
            .expect("delegate returns a terminal tool result");

        assert!(!result.success, "strict mixed chain must fail: {result:?}");
        assert!(
            result.output.is_empty(),
            "terminal failure has no output: {result:?}"
        );
        let expected =
            crate::i18n::get_required_cli_string("turn-tool-protocol-strict-mixed-error");
        let error = result.error.expect("strict mixed chain returns an error");
        assert!(error.contains(&expected), "unexpected error: {error}");
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "strict mixed validation must precede the primary request"
        );
        assert_eq!(
            backup_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "strict mixed validation must precede fallback dispatch"
        );
    }

    #[tokio::test]
    async fn non_agentic_delegate_preserves_generic_fallback_warning() {
        let (primary, primary_requests) = start_failing_chat_server(503).await;
        let backup = start_final_chat_server(vec!["non-agentic fallback reply"]).await;
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), false);
        let tool = fallback_delegate_tool(config, None);

        let (result, outer_fallback) =
            zeroclaw_providers::reliable::scope_provider_fallback(async {
                let result = tool
                    .execute(json!({"agent": "target", "prompt": "respond"}))
                    .await
                    .expect("delegate call completes");
                let outer_fallback = zeroclaw_providers::reliable::take_last_provider_fallback();
                (result, outer_fallback)
            })
            .await;

        assert!(result.success, "fallback should recover: {result:?}");
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must be attempted exactly once"
        );
        assert!(
            result.output.contains("non-agentic fallback reply"),
            "{result:?}"
        );
        assert_generic_fallback_warning(result.output.as_str(), &primary.uri);
        assert_explicit_fallback_attribution(result.output.as_str(), false);
        assert!(
            outer_fallback.is_none(),
            "delegate fallback must not leak into the parent channel scope: {outer_fallback:?}"
        );
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn background_delegate_persists_generic_fallback_warning() {
        let (primary, primary_requests) = start_failing_chat_server(503).await;
        let backup = start_final_chat_server(vec!["background fallback reply"]).await;
        let workspace = TempDir::new().expect("temporary workspace");
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), true);
        let tool = fallback_delegate_tool(config, Some(workspace.path().to_path_buf()));

        let (start, outer_fallback) =
            zeroclaw_providers::reliable::scope_provider_fallback(async {
                let start = tool
                    .execute(json!({
                        "agent": "target",
                        "prompt": "respond",
                        "background": true,
                    }))
                    .await
                    .expect("background delegate starts");
                let outer_fallback = zeroclaw_providers::reliable::take_last_provider_fallback();
                (start, outer_fallback)
            })
            .await;

        assert!(start.success, "background start failed: {start:?}");
        assert!(
            outer_fallback.is_none(),
            "background delegate fallback must not leak into its parent scope: {outer_fallback:?}"
        );
        let task_id = start
            .output
            .lines()
            .find_map(|line| line.strip_prefix("task_id: "))
            .expect("background start includes task id");
        let result = wait_for_terminal_background_result(workspace.path(), task_id).await;

        assert_eq!(result.status, BackgroundTaskStatus::Completed, "{result:?}");
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must be attempted exactly once"
        );
        let output = result.output.as_deref().expect("completed output");
        assert!(output.contains("background fallback reply"), "{result:?}");
        assert_generic_fallback_warning(output, &primary.uri);
        assert_explicit_fallback_attribution(output, true);
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn parallel_delegate_preserves_generic_fallback_warning() {
        let (primary, primary_requests) = start_failing_chat_server(503).await;
        let backup = start_final_chat_server(vec!["parallel fallback reply"]).await;
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), true);
        let tool = fallback_delegate_tool(config, None);

        let (result, outer_fallback) =
            zeroclaw_providers::reliable::scope_provider_fallback(async {
                let result = tool
                    .execute(json!({"parallel": ["target"], "prompt": "respond"}))
                    .await
                    .expect("parallel delegate completes");
                let outer_fallback = zeroclaw_providers::reliable::take_last_provider_fallback();
                (result, outer_fallback)
            })
            .await;

        assert!(result.success, "parallel delegate failed: {result:?}");
        assert!(
            outer_fallback.is_none(),
            "parallel delegate fallback must not leak into the parent scope: {outer_fallback:?}"
        );
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must be attempted exactly once"
        );
        assert!(
            result.output.contains("parallel fallback reply"),
            "{result:?}"
        );
        assert_generic_fallback_warning(result.output.as_str(), &primary.uri);
        assert_explicit_fallback_attribution(result.output.as_str(), true);
        assert!(result.error.is_none(), "{result:?}");
    }

    #[tokio::test]
    async fn delegate_returns_safe_ordered_summary_when_fallbacks_exhaust() {
        let (primary, primary_requests) =
            start_failing_chat_server_with_error(503, "primary failure marker").await;
        let (backup, backup_requests) =
            start_failing_chat_server_with_error(503, "backup failure marker").await;
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), true);
        let tool = fallback_delegate_tool(config, None);

        let (result, outer_fallback) =
            zeroclaw_providers::reliable::scope_provider_fallback(async {
                let result = tool
                    .execute(json!({"agent": "target", "prompt": "respond"}))
                    .await
                    .expect("delegate call completes");
                let outer_fallback = zeroclaw_providers::reliable::take_last_provider_fallback();
                (result, outer_fallback)
            })
            .await;

        assert!(!result.success, "exhaustion must be terminal: {result:?}");
        assert!(
            result.output.is_empty(),
            "terminal error must not carry output: {result:?}"
        );
        assert!(
            outer_fallback.is_none(),
            "failed delegation must not leave recovery metadata in the parent scope: {outer_fallback:?}"
        );
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must be attempted exactly once"
        );
        assert_eq!(
            backup_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the configured fallback must be attempted exactly once"
        );
        let error = result.error.expect("terminal delegate error");
        assert!(
            error.contains("All model providers/models failed after 2 failure event(s)"),
            "terminal error summarizes every failure event: {error}"
        );
        assert!(
            error.contains("event 1 (retry 1/1): retryable")
                && error.contains("event 2 (retry 1/1): retryable"),
            "summary preserves event order: {error}"
        );
        assert!(
            !error.contains("primary failure marker") && !error.contains("backup failure marker"),
            "provider-controlled response bodies must not reach the caller: {error}"
        );
        let warning = crate::i18n::get_required_cli_string("delegate-provider-fallback-warning");
        assert!(
            !error.contains(&warning),
            "terminal errors must remain errors rather than recovery warnings: {error}"
        );
    }

    #[tokio::test]
    async fn delegate_timeout_stays_distinct_from_provider_exhaustion() {
        let (primary, _primary_requests) = start_slow_chat_server(Duration::from_secs(2)).await;
        let (backup, backup_requests) = start_failing_chat_server(503).await;
        let (fixture_config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), false);
        let mut config = (*fixture_config).clone();
        config
            .runtime_profiles
            .get_mut("review")
            .expect("review runtime profile")
            .delegation_timeout_secs = Some(1);
        let tool = fallback_delegate_tool(Arc::new(config), None);

        let result = tool
            .execute(json!({"agent": "target", "prompt": "respond"}))
            .await
            .expect("delegate call completes");

        assert!(!result.success, "timeout must remain terminal: {result:?}");
        assert!(
            result.output.is_empty(),
            "timeout must not carry output: {result:?}"
        );
        let error = result.error.expect("timeout error");
        assert_eq!(error, "Agent 'target' timed out after 1s");
        assert_eq!(
            backup_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "outer timeout must not be converted into fallback exhaustion"
        );
    }

    #[tokio::test]
    async fn background_delegate_persists_safe_summary_when_fallbacks_exhaust() {
        let (primary, primary_requests) =
            start_failing_chat_server_with_error(503, "background primary failure marker").await;
        let (backup, backup_requests) =
            start_failing_chat_server_with_error(503, "background backup failure marker").await;
        let workspace = TempDir::new().expect("temporary workspace");
        let (config, _fixture_dir) =
            fallback_delegate_config(primary.uri.clone(), backup.uri.clone(), true);
        let tool = fallback_delegate_tool(config, Some(workspace.path().to_path_buf()));

        let start = tool
            .execute(json!({
                "agent": "target",
                "prompt": "respond",
                "background": true,
            }))
            .await
            .expect("background delegate starts");
        let task_id = start
            .output
            .lines()
            .find_map(|line| line.strip_prefix("task_id: "))
            .expect("background start includes task id");
        let persisted = wait_for_terminal_background_result(workspace.path(), task_id).await;

        assert_eq!(
            persisted.status,
            BackgroundTaskStatus::Failed,
            "{persisted:?}"
        );
        assert_eq!(
            primary_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the controlled primary must be attempted exactly once"
        );
        assert_eq!(
            backup_requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the configured fallback must be attempted exactly once"
        );
        let persisted_error = persisted
            .error
            .as_deref()
            .expect("persisted failure detail");
        assert!(
            persisted_error.contains("All model providers/models failed after 2 failure event(s)")
                && persisted_error.contains("event 1 (retry 1/1): retryable")
                && persisted_error.contains("event 2 (retry 1/1): retryable"),
            "persisted error must contain the safe ordered summary: {persisted_error}"
        );
        assert!(
            !persisted_error.contains("background primary failure marker")
                && !persisted_error.contains("background backup failure marker"),
            "provider-controlled response bodies must not persist: {persisted_error}"
        );

        let result = tool
            .execute(json!({"action": "check_result", "task_id": task_id}))
            .await
            .expect("check_result completes");
        assert!(
            !result.success,
            "terminal background failure is not success: {result:?}"
        );
        let error = result
            .error
            .expect("caller receives background failure detail");
        assert!(
            error.contains("All model providers/models failed after 2 failure event(s)")
                && error.contains("event 1 (retry 1/1): retryable")
                && error.contains("event 2 (retry 1/1): retryable"),
            "check_result returns the same safe summary: {error}"
        );
        assert!(
            !error.contains("background primary failure marker")
                && !error.contains("background backup failure marker"),
            "provider-controlled response bodies must not reach check_result: {error}"
        );
        let warning = crate::i18n::get_required_cli_string("delegate-provider-fallback-warning");
        assert!(
            !error.contains(&warning),
            "terminal background errors must not become recovery warnings: {error}"
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
            .with_runtime(Arc::new(DelegateTestRuntime))
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
            .with_runtime(Arc::new(DelegateTestRuntime))
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
            .with_runtime(Arc::new(DelegateTestRuntime))
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

    /// Records the tool names offered to the delegated turn, so a regression can
    /// assert both presence and absence by name.
    struct RecordingToolNamesProvider {
        seen: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for RecordingToolNamesProvider {
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
            Ok("unused".into())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if let Some(tools) = request.tools {
                let mut seen = self.seen.lock().unwrap();
                for tool in tools {
                    seen.push(tool.name.clone());
                }
            }
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for RecordingToolNamesProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "RecordingToolNamesProvider"
        }
    }

    /// A parent-registry tool fixture with an arbitrary name, standing in for
    /// any tool that is neither rebuilt against the target policy nor proven
    /// safe to reuse.
    /// A REAL MCP tool: a wrapper over a registry that actually routes it.
    ///
    /// Name-only stand-ins no longer stand in for one. Bounded admission
    /// resolves the owning server through the registry the instance carries,
    /// because a prefixed name cannot be decoded back into a server whose own
    /// name may contain the separator - so a fixture that only carries the
    /// name can no longer express which server owns the tool.
    fn mcp_fixture_tool(server: &str, tool: &str) -> Arc<dyn Tool> {
        let registry = Arc::new(
            zeroclaw_tools::mcp_client::McpRegistry::for_test_with_echoing_tool(server, tool),
        );
        let def = zeroclaw_tools::mcp_protocol::McpToolDef {
            name: tool.to_string(),
            description: Some("fixture MCP tool".to_string()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        Arc::new(crate::tools::McpToolWrapper::new(
            format!("{server}__{tool}"),
            def,
            registry,
            Arc::new(SecurityPolicy::default()),
        ))
    }

    struct NamedFixtureTool(&'static str);

    impl ::zeroclaw_api::attribution::Attributable for NamedFixtureTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            self.0
        }
    }

    #[async_trait]
    impl Tool for NamedFixtureTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "Test-only parent-registry fixture"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            unreachable!("bounded reuse regressions never execute the fixture")
        }
    }

    /// Builds a caller/target pair for the bounded-reuse regressions.
    ///
    /// `same_risk_profile` puts both agents on ONE risk profile and lets them
    /// reach each other through `delegate_same_risk_profile`. That is as close
    /// to "no boundary crossed" as the config layer can get: a target can never
    /// be the caller itself, because both reachability paths skip the caller's
    /// own alias.
    fn bounded_reuse_config(
        tool_names: &[&str],
        same_risk_profile: bool,
        tmp: &TempDir,
    ) -> Arc<zeroclaw_config::schema::Config> {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let mut allowed: Vec<String> = tool_names.iter().map(|n| n.to_string()).collect();
        allowed.push(DelegateTool::NAME.to_string());

        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: allowed.clone(),
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: allowed,
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
                runtime_profile: "agentic".into(),
                model_provider: "ollama.caller".into(),
                // Same-profile peers are reached implicitly, so the explicit
                // list stays empty in that case.
                delegates: if same_risk_profile {
                    Vec::new()
                } else {
                    vec![DelegateTargetConfig::bounded("target")]
                },
                delegate_same_risk_profile: same_risk_profile,
                ..AliasedAgentConfig::default()
            },
        );
        // A DIFFERENT risk profile is what makes the pair cross a policy
        // boundary; sharing one leaves only the alias boundary, which every
        // delegation crosses.
        let target_profile = if same_risk_profile {
            "caller"
        } else {
            "target"
        };
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: target_profile.into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                ..AliasedAgentConfig::default()
            },
        );
        Arc::new(config)
    }

    /// Runs a bounded delegation and returns the tool names the target turn was
    /// actually offered.
    async fn bounded_offered_tool_names(
        config: &Arc<zeroclaw_config::schema::Config>,
        target_alias: &str,
        parent_tools: Vec<Arc<dyn Tool>>,
    ) -> Vec<String> {
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, caller_policy)
            .with_root_config(Arc::clone(config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(parent_tools)));

        let target_config = config
            .agents
            .get(target_alias)
            .expect("target agent exists")
            .clone();

        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingToolNamesProvider {
            seen: Arc::clone(&seen),
        };
        let result = tool
            .execute_agentic(
                target_alias,
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "do the thing",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        seen.lock().unwrap().clone()
    }

    /// D1 core: a caller tool that is neither rebuilt against the target policy
    /// nor listed in `SAFE_FOR_BOUNDED_REUSE` must be OMITTED from a
    /// cross-profile bounded target, not handed over as the caller instance.
    #[tokio::test]
    async fn bounded_cross_profile_omits_unclassified_caller_tool() {
        let tmp = TempDir::new().unwrap();
        let config = bounded_reuse_config(&["unclassified_fixture"], false, &tmp);

        let names = bounded_offered_tool_names(
            &config,
            "target",
            vec![Arc::new(NamedFixtureTool("unclassified_fixture"))],
        )
        .await;

        assert!(
            !names.iter().any(|n| n == "unclassified_fixture"),
            "an unclassified caller tool must not be inherited by a cross-profile \
             bounded target, got {names:?}"
        );
    }

    /// The positive side of the same rule: a name proven free of caller capture
    /// is still reused, so the inversion costs no functionality.
    #[tokio::test]
    async fn bounded_cross_profile_keeps_safe_for_reuse_caller_tool() {
        let tmp = TempDir::new().unwrap();
        let config = bounded_reuse_config(&["calculator"], false, &tmp);

        let names = bounded_offered_tool_names(
            &config,
            "target",
            vec![Arc::new(crate::tools::CalculatorTool::new())],
        )
        .await;

        assert!(
            names.iter().any(|n| n == "calculator"),
            "a SAFE_FOR_BOUNDED_REUSE tool must survive cross-profile bounded \
             delegation, got {names:?}"
        );
    }

    /// Sharing a risk profile is NOT a reason to hand over caller instances.
    ///
    /// Two same-profile agents still have different aliases, and the
    /// identity-bound tools capture the alias rather than the policy - so this
    /// pair crosses a real boundary even though `policy_for_target` hands the
    /// target the caller's workspace on purpose. A target can never be the
    /// caller itself either: both reachability paths skip the caller's own
    /// alias. So the rule keys off nothing narrower than "bounded delegation
    /// with a resolvable root config".
    #[tokio::test]
    async fn bounded_same_risk_profile_still_omits_unclassified_caller_tool() {
        let tmp = TempDir::new().unwrap();
        let config = bounded_reuse_config(&["unclassified_fixture"], true, &tmp);

        let caller_policy = SecurityPolicy::for_agent(&config, "caller").unwrap();
        let target_policy = SecurityPolicy::for_agent(&config, "target").unwrap();
        assert_eq!(
            caller_policy.risk_profile_name, target_policy.risk_profile_name,
            "the fixture must put both agents on one risk profile"
        );

        let names = bounded_offered_tool_names(
            &config,
            "target",
            vec![Arc::new(NamedFixtureTool("unclassified_fixture"))],
        )
        .await;

        assert!(
            !names.iter().any(|n| n == "unclassified_fixture"),
            "sharing a risk profile must not let an unclassified caller tool through, got {names:?}"
        );
    }

    /// D3/C-9: an MCP tool is admitted for a bounded target when the TARGET's
    /// own `mcp_bundles` grant the server that prefixes it, and omitted when
    /// only the caller has that server.
    ///
    /// The rule matches against the target's resolved server list rather than
    /// splitting the name on `__`, because nothing constrains a server name
    /// from containing `__` itself.
    #[tokio::test]
    async fn bounded_cross_profile_admits_only_target_granted_mcp_servers() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, McpBundleConfig, McpServerConfig, RiskProfileConfig,
            RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };

        // Both servers exist in the registry; the bundles decide who gets which.
        config.mcp.servers = vec![
            McpServerConfig {
                name: "shared_srv".to_string(),
                ..McpServerConfig::default()
            },
            McpServerConfig {
                name: "caller_srv".to_string(),
                ..McpServerConfig::default()
            },
        ];
        config.mcp_bundles.insert(
            "shared_bundle".to_string(),
            McpBundleConfig {
                servers: vec!["shared_srv".to_string()],
                exclude: Vec::new(),
            },
        );
        config.mcp_bundles.insert(
            "caller_bundle".to_string(),
            McpBundleConfig {
                servers: vec!["caller_srv".to_string()],
                exclude: Vec::new(),
            },
        );

        // Both MCP names are admitted by the caller's own policy, so what keeps
        // the caller-only server out of the target registry is the target's
        // bundle grant, not `is_tool_allowed` (which, unlike
        // `delegate_admits_with_mcp`, matches names literally).
        let allowed = vec![
            DelegateTool::NAME.to_string(),
            "shared_srv__do_thing".to_string(),
            "caller_srv__do_thing".to_string(),
        ];
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: allowed.clone(),
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: allowed,
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
                runtime_profile: "agentic".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig::bounded("target")],
                mcp_bundles: vec!["shared_bundle".to_string(), "caller_bundle".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                // The target is granted ONLY the shared server.
                mcp_bundles: vec!["shared_bundle".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);

        let names = bounded_offered_tool_names(
            &config,
            "target",
            vec![
                mcp_fixture_tool("shared_srv", "do_thing"),
                mcp_fixture_tool("caller_srv", "do_thing"),
            ],
        )
        .await;

        assert!(
            names.iter().any(|n| n == "shared_srv__do_thing"),
            "an MCP tool whose server the target's own bundles grant must survive, \
             got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "caller_srv__do_thing"),
            "an MCP tool whose server only the caller holds must be omitted, \
             got {names:?}"
        );
    }

    /// Calls one named tool, then reports the tool result as the final answer,
    /// so a regression can assert on what the TARGET's own instance returned.
    struct SingleToolCallCapturingProvider {
        tool_name: String,
        arguments: String,
        tool_message: std::sync::Mutex<Option<String>>,
        saw_tool: std::sync::Mutex<bool>,
    }

    impl SingleToolCallCapturingProvider {
        fn new(tool_name: &str, arguments: serde_json::Value) -> Self {
            Self {
                tool_name: tool_name.to_string(),
                arguments: arguments.to_string(),
                tool_message: std::sync::Mutex::new(None),
                saw_tool: std::sync::Mutex::new(false),
            }
        }

        fn tool_message(&self) -> Option<String> {
            self.tool_message.lock().unwrap().clone()
        }

        fn tool_was_offered(&self) -> bool {
            *self.saw_tool.lock().unwrap()
        }
    }

    #[async_trait]
    impl ModelProvider for SingleToolCallCapturingProvider {
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
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let offered = request
                .tools
                .is_some_and(|tools| tools.iter().any(|tool| tool.name == self.tool_name));
            if offered {
                *self.saw_tool.lock().unwrap() = true;
            }
            if let Some(tool_message) = request.messages.iter().find(|m| m.role == "tool") {
                *self.tool_message.lock().unwrap() = Some(tool_message.content.clone());
                return Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                });
            }
            if !offered {
                // Calling a tool the target was never offered would fail the
                // turn on a semantic error and hide which assertion actually
                // caught the regression.
                return Ok(ChatResponse {
                    text: Some("tool not offered".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: self.tool_name.clone(),
                    arguments: self.arguments.clone(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for SingleToolCallCapturingProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "SingleToolCallCapturingProvider"
        }
    }

    /// Caller/target pair whose only difference is the autonomy level, so a
    /// regression can tell WHICH policy a rebuilt tool ended up gating on.
    fn autonomy_rebound_config(
        tool_name: &str,
        target_level: zeroclaw_config::autonomy::AutonomyLevel,
        tmp: &TempDir,
    ) -> Arc<zeroclaw_config::schema::Config> {
        use zeroclaw_config::autonomy::{AutonomyLevel, DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let allowed = vec![tool_name.to_string(), DelegateTool::NAME.to_string()];

        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                // The caller may act.
                level: AutonomyLevel::Full,
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: allowed.clone(),
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                level: target_level,
                allowed_tools: allowed,
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
                runtime_profile: "agentic".into(),
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
        Arc::new(config)
    }

    /// A read-only target must not act with the caller's autonomy.
    ///
    /// `http_request` bakes the `SecurityPolicy` it was built with into a
    /// private field and checks `can_act()` before it does anything else, so a
    /// caller-built instance handed to a read-only target would act under the
    /// CALLER's autonomy. The rebuilt instance must refuse instead - and it
    /// must still be present, since omitting it is a loss of function, not a
    /// fix.
    ///
    /// No network is reachable from this test either way: the gate returns
    /// before any connection is attempted, and the URL points at a closed
    /// loopback port so a regression fails locally rather than dialing out.
    #[tokio::test]
    async fn bounded_cross_profile_http_request_gates_on_target_autonomy() {
        let tmp = TempDir::new().unwrap();
        let config = autonomy_rebound_config(
            "http_request",
            zeroclaw_config::autonomy::AutonomyLevel::ReadOnly,
            &tmp,
        );

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        assert!(
            caller_policy.can_act(),
            "the caller fixture must be able to act, or the test proves nothing"
        );
        let target_policy =
            SecurityPolicy::for_agent(&config, "target").expect("target policy resolves");
        assert!(
            !target_policy.can_act(),
            "the target fixture must be read-only"
        );

        // Built with the CALLER's policy, exactly as the real registry does.
        let caller_http = crate::tools::HttpRequestTool::new_with_config(
            Arc::clone(&caller_policy),
            vec!["127.0.0.1".to_string()],
            1024,
            1,
            true,
            vec!["127.0.0.1".to_string()],
            Vec::new(),
            config.config_path.clone(),
            false,
        )
        .expect("caller http_request builds");

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(
                crate::tools::RateLimitedTool::new(caller_http, Arc::clone(&caller_policy)),
            )])));

        let target_config = config
            .agents
            .get("target")
            .expect("target agent exists")
            .clone();

        let provider = SingleToolCallCapturingProvider::new(
            "http_request",
            json!({"url": "http://127.0.0.1:9/", "method": "GET"}),
        );
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "fetch it",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        assert!(
            provider.tool_was_offered(),
            "http_request must stay available to the target: rebuilding it against \
             the target's policy is the fix, omitting it is a loss of function"
        );
        let observed = provider
            .tool_message()
            .expect("the target's http_request must have produced a tool result");
        assert!(
            observed.contains("Action blocked: autonomy is read-only"),
            "http_request must gate on the TARGET's autonomy, not the caller's; got {observed:?}"
        );
    }

    /// Control case for the regression above: rebuilding against the target's
    /// policy must restore the capability, not quietly disable it. A target
    /// that MAY act gets an `http_request` that passes its own autonomy gate.
    ///
    /// The URL is a closed loopback port, so the call fails at the connection,
    /// never off-box - what matters is only that it got past the gate.
    #[tokio::test]
    async fn bounded_cross_profile_http_request_stays_usable_for_acting_target() {
        let tmp = TempDir::new().unwrap();
        let config = autonomy_rebound_config(
            "http_request",
            zeroclaw_config::autonomy::AutonomyLevel::Full,
            &tmp,
        );

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let caller_http = crate::tools::HttpRequestTool::new_with_config(
            Arc::clone(&caller_policy),
            vec!["127.0.0.1".to_string()],
            1024,
            1,
            true,
            vec!["127.0.0.1".to_string()],
            Vec::new(),
            config.config_path.clone(),
            false,
        )
        .expect("caller http_request builds");

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(
                crate::tools::RateLimitedTool::new(caller_http, Arc::clone(&caller_policy)),
            )])));

        let target_config = config
            .agents
            .get("target")
            .expect("target agent exists")
            .clone();

        let provider = SingleToolCallCapturingProvider::new(
            "http_request",
            json!({"url": "http://127.0.0.1:9/", "method": "GET"}),
        );
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "fetch it",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        assert!(
            provider.tool_was_offered(),
            "http_request must remain available to a target that may act"
        );
        let observed = provider
            .tool_message()
            .expect("the target's http_request must have produced a tool result");
        assert!(
            !observed.contains("Action blocked: autonomy is read-only"),
            "a target that may act must not be gated as read-only; got {observed:?}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_cron_run_cannot_trigger_callers_job() {
        // `cron_run` resolves the job through
        // `cron::get_job_for_agent(.., &self.agent_alias)` before it runs
        // anything, so an instance built for the CALLER lets a Bounded target
        // fire the caller's jobs under the caller's stored identity. The
        // rebuilt instance must not resolve them at all.
        //
        // Safe either way: resolution fails before execution, and the job's
        // command names an executable that does not exist, so even a total
        // regression could not run anything meaningful.
        let fixture = bounded_delegate_full_fixture("cron_run", |_cfg| {}).await;

        crate::cron::add_shell_job(
            &fixture.config,
            "executive_assistant",
            Some("callers_probe".to_string()),
            crate::cron::Schedule::Every {
                every_ms: 3_600_000,
            },
            "zzz_nonexistent_command_for_9872",
        )
        .unwrap();
        let callers_job = crate::cron::list_jobs_by_agent(&fixture.config, "executive_assistant")
            .unwrap()
            .into_iter()
            .next()
            .expect("the caller owns one job");

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "cron_run",
            tool_args: serde_json::json!({ "job_id": callers_job.id }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "run that job",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "cron_run must stay available to the target - omitting it is a loss \
             of function, not a fix: {result:?}"
        );
        let runs = crate::cron::list_runs(&fixture.config, &callers_job.id, 10).unwrap();
        assert!(
            runs.is_empty(),
            "regression: a Bounded delegate target triggered the CALLER's cron job - \
             got runs: {runs:?}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_cron_list_shows_target_jobs_not_callers() {
        // `cron_list` enumerates through
        // `cron::list_jobs_by_agent(.., &self.agent_alias)`, so the caller's
        // instance shows a Bounded target the CALLER's schedule.
        let fixture = bounded_delegate_full_fixture("cron_list", |_cfg| {}).await;

        crate::cron::add_shell_job(
            &fixture.config,
            "executive_assistant",
            Some("callers_only_job".to_string()),
            crate::cron::Schedule::Every {
                every_ms: 3_600_000,
            },
            "zzz_nonexistent_command_for_9872",
        )
        .unwrap();
        crate::cron::add_shell_job(
            &fixture.config,
            "fs_researcher",
            Some("targets_own_job".to_string()),
            crate::cron::Schedule::Every {
                every_ms: 3_600_000,
            },
            "zzz_nonexistent_command_for_9872",
        )
        .unwrap();

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "cron_list",
            tool_args: serde_json::json!({}),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "list the jobs",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "cron_list must stay available to the target: {result:?}"
        );
        let output = result.output.to_string();
        assert!(
            output.contains("targets_own_job"),
            "the target must see its OWN jobs - got: {output}"
        );
        assert!(
            !output.contains("callers_only_job"),
            "regression: a Bounded delegate target enumerated the CALLER's cron \
             jobs - got: {output}"
        );
    }

    #[tokio::test]
    async fn bounded_delegate_cron_runs_cannot_read_callers_history() {
        // `cron_runs` reads history through the same
        // `cron::get_job_for_agent(.., &self.agent_alias)` resolution as
        // `cron_run`, so the caller's instance exposes the CALLER's run log.
        let fixture = bounded_delegate_full_fixture("cron_runs", |_cfg| {}).await;

        crate::cron::add_shell_job(
            &fixture.config,
            "executive_assistant",
            Some("callers_probe".to_string()),
            crate::cron::Schedule::Every {
                every_ms: 3_600_000,
            },
            "zzz_nonexistent_command_for_9872",
        )
        .unwrap();
        let callers_job = crate::cron::list_jobs_by_agent(&fixture.config, "executive_assistant")
            .unwrap()
            .into_iter()
            .next()
            .expect("the caller owns one job");

        let model_provider = BoundedSingleToolCallThenFinalModelProvider {
            tool_name: "cron_runs",
            tool_args: serde_json::json!({ "job_id": callers_job.id }),
        };

        let result = fixture
            .tool
            .execute_agentic(
                "fs_researcher",
                &fixture.target_config,
                "custom",
                "delegate-fs-test-model",
                &model_provider,
                "show the run history",
                Some(0.2),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "cron_runs must stay available to the target: {result:?}"
        );
        let output = result.output.to_string();
        assert!(
            !output.contains("callers_probe"),
            "regression: a Bounded delegate target read the CALLER's cron run \
             history - got: {output}"
        );
    }

    /// `ask_user` gates on the policy it was built with, and the gate lives
    /// inside the tool (not in the `RateLimitedTool` wrapper), so re-wrapping
    /// the caller's instance would change nothing. The rebuilt instance must
    /// gate on the TARGET's autonomy - and must still be present, keeping the
    /// caller's live channel handle so it can actually reach someone.
    #[tokio::test]
    async fn bounded_cross_profile_ask_user_gates_on_target_autonomy() {
        let tmp = TempDir::new().unwrap();
        let config = autonomy_rebound_config(
            "ask_user",
            zeroclaw_config::autonomy::AutonomyLevel::ReadOnly,
            &tmp,
        );

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let handle: crate::tools::PerToolChannelHandle =
            Arc::new(RwLock::new(std::collections::HashMap::new()));
        let caller_ask_user =
            crate::tools::ask_user_tool(Arc::clone(&caller_policy), Arc::clone(&handle));

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_channel_handles(crate::tools::DelegateChannelHandles {
                ask_user: Some(Arc::clone(&handle)),
                ..Default::default()
            })
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_ask_user])));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let provider = SingleToolCallCapturingProvider::new(
            "ask_user",
            json!({"question": "ready?", "channel": "test"}),
        );
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "ask them",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        assert!(
            provider.tool_was_offered(),
            "ask_user must stay available: it is rebuilt with the caller's live \
             handle, not dropped"
        );
        let observed = provider
            .tool_message()
            .expect("the target's ask_user must have produced a tool result");
        assert!(
            observed.contains("Action blocked"),
            "ask_user must gate on the TARGET's autonomy, not the caller's; got {observed:?}"
        );
    }

    /// Without the caller's channel handle there is no correct instance to give
    /// the target, and building one with a fresh empty map would advertise a
    /// tool that cannot reach any channel. Fail closed: omit it.
    #[tokio::test]
    async fn bounded_cross_profile_omits_channel_tool_when_handle_is_absent() {
        let tmp = TempDir::new().unwrap();
        let config = autonomy_rebound_config(
            "ask_user",
            zeroclaw_config::autonomy::AutonomyLevel::Full,
            &tmp,
        );

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let handle: crate::tools::PerToolChannelHandle =
            Arc::new(RwLock::new(std::collections::HashMap::new()));
        let caller_ask_user = crate::tools::ask_user_tool(Arc::clone(&caller_policy), handle);

        // No `with_channel_handles` at all.
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_ask_user])));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let provider =
            SingleToolCallCapturingProvider::new("ask_user", json!({"question": "ready?"}));
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "ask them",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        assert!(
            !provider.tool_was_offered(),
            "with no channel handle plumbed through, ask_user must be omitted \
             rather than handed over as the caller's instance"
        );
    }

    /// `llm_task` bakes the resolved provider's api_key into a field, so a
    /// target that resolves no provider of its own must NOT inherit the
    /// caller's instance - that would hand over the caller's credential. It is
    /// omitted instead, which fails before any network call rather than after.
    #[tokio::test]
    async fn bounded_cross_profile_omits_llm_task_when_target_resolves_no_provider() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig,
            RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        // Only the CALLER's provider exists, and it carries a secret.
        config.providers.models.custom.insert(
            "callers-own".to_string(),
            CustomModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("caller-model".to_string()),
                    api_key: Some("sk-caller-only-credential".to_string()),
                    ..Default::default()
                },
            },
        );
        let allowed = vec!["llm_task".to_string(), DelegateTool::NAME.to_string()];
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: allowed.clone(),
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: allowed,
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
                runtime_profile: "agentic".into(),
                model_provider: "custom.callers-own".into(),
                delegates: vec![DelegateTargetConfig::bounded("target")],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                // Names a provider that is not configured: resolves to None.
                model_provider: "custom.absent".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let caller_llm_task =
            crate::tools::llm_task_tool(Arc::clone(&caller_policy), &config, "caller")
                .expect("the caller resolves its own provider");

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_llm_task])));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let names = {
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let provider = RecordingToolNamesProvider {
                seen: Arc::clone(&seen),
            };
            let result = tool
                .execute_agentic(
                    "target",
                    &target_config,
                    "ollama",
                    "test-model",
                    &provider,
                    "do the thing",
                    None,
                )
                .await
                .expect("bounded delegation runs");
            assert!(result.success, "delegation failed: {:?}", result.error);
            seen.lock().unwrap().clone()
        };

        assert!(
            !names.iter().any(|n| n == "llm_task"),
            "a target that resolves no provider must not inherit the caller's \
             llm_task, which carries the caller's api_key; got {names:?}"
        );
    }

    /// Approval authority cannot be rebuilt for the target - it may simply not
    /// be in the checkpoint's required group - so `sop_approve` is replaced by
    /// a stub that refuses without touching the engine.
    #[tokio::test]
    async fn bounded_cross_profile_sop_approve_is_replaced_by_refusing_stub() {
        let tmp = TempDir::new().unwrap();
        let config = autonomy_rebound_config(
            "sop_approve",
            zeroclaw_config::autonomy::AutonomyLevel::Full,
            &tmp,
        );

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let engine = Arc::new(std::sync::Mutex::new(crate::sop::SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        )));
        let caller_sop_approve: Arc<dyn Tool> = Arc::new(
            crate::tools::SopApproveTool::new(Arc::clone(&engine)).with_agent_alias("caller"),
        );

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_sop_approve])));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let provider =
            SingleToolCallCapturingProvider::new("sop_approve", json!({"run_id": "some-run-id"}));
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "approve it",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        assert!(
            provider.tool_was_offered(),
            "sop_approve stays in the registry as a stub, so the model is told it \
             may not approve rather than finding no tool at all"
        );
        let observed = provider
            .tool_message()
            .expect("the stub must have produced a tool result");
        assert!(
            observed.contains("not available to a bounded delegate target"),
            "sop_approve must refuse for a bounded target; got {observed:?}"
        );
    }

    /// The positive half of the `llm_task` pair. The test above asserts the
    /// caller's instance is not inherited, which the deny-by-default fallback
    /// would satisfy on its own; this one fails unless the tool is actually
    /// REBUILT, and it is rebuilt from `agent_name`, so the provider and
    /// api_key it bakes in are the target's own.
    #[tokio::test]
    async fn bounded_cross_profile_rebuilds_llm_task_for_target_with_own_provider() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, CustomModelProviderConfig, ModelProviderConfig,
            RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        for (alias, model, key) in [
            ("callers-own", "caller-model", "sk-caller-only-credential"),
            ("targets-own", "target-model", "sk-target-only-credential"),
        ] {
            config.providers.models.custom.insert(
                alias.to_string(),
                CustomModelProviderConfig {
                    base: ModelProviderConfig {
                        model: Some(model.to_string()),
                        api_key: Some(key.to_string()),
                        ..Default::default()
                    },
                },
            );
        }
        let allowed = vec!["llm_task".to_string(), DelegateTool::NAME.to_string()];
        config.risk_profiles.insert(
            "caller".to_string(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: allowed.clone(),
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "target".to_string(),
            RiskProfileConfig {
                allowed_tools: allowed,
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
                runtime_profile: "agentic".into(),
                model_provider: "custom.callers-own".into(),
                delegates: vec![DelegateTargetConfig::bounded("target")],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "custom.targets-own".into(),
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let caller_llm_task =
            crate::tools::llm_task_tool(Arc::clone(&caller_policy), &config, "caller")
                .expect("the caller resolves its own provider");

        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_llm_task])));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let provider = RecordingToolNamesProvider {
            seen: Arc::clone(&seen),
        };
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "do the thing",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        let names = seen.lock().unwrap().clone();
        assert!(
            names.iter().any(|n| n == "llm_task"),
            "a target with its own provider must get llm_task rebuilt, not dropped; \
             got {names:?}"
        );
    }

    /// Every explicitly denied name is actually absent from a cross-profile
    /// bounded target's registry. The deny-by-default fallback is what enforces
    /// this, so the test would pass without the constant - its value is that it
    /// fails the day someone reclassifies one of these as reusable.
    #[tokio::test]
    async fn bounded_cross_profile_omits_every_explicitly_denied_tool() {
        for denied in crate::tools::BOUNDED_DENIED_TOOL_NAMES {
            let tmp = TempDir::new().unwrap();
            let config = bounded_reuse_config(&[denied], false, &tmp);

            let names = bounded_offered_tool_names(
                &config,
                "target",
                vec![Arc::new(NamedFixtureTool(denied))],
            )
            .await;

            assert!(
                !names.iter().any(|n| n == denied),
                "'{denied}' is in BOUNDED_DENIED_TOOL_NAMES but reached a \
                 cross-profile bounded target; got {names:?}"
            );
        }
    }

    /// Captures the system prompt the delegated turn was given.
    struct SystemPromptCapturingProvider {
        prompt: std::sync::Mutex<Option<String>>,
    }

    impl SystemPromptCapturingProvider {
        fn new() -> Self {
            Self {
                prompt: std::sync::Mutex::new(None),
            }
        }

        fn captured(&self) -> Option<String> {
            self.prompt.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for SystemPromptCapturingProvider {
        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat_with_system(
            &self,
            system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            if let Some(prompt) = system_prompt {
                *self.prompt.lock().unwrap() = Some(prompt.to_string());
            }
            Ok("done".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if let Some(system) = request.messages.iter().find(|m| m.role == "system") {
                *self.prompt.lock().unwrap() = Some(system.content.clone());
            }
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for SystemPromptCapturingProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "SystemPromptCapturingProvider"
        }
    }

    /// Writes one skill into `<workspace>/skills/<name>/SKILL.md`.
    fn write_skill(workspace: &std::path::Path, name: &str, description: &str) {
        let dir = crate::skills::skills_dir(workspace).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody for {name}.\n"),
        )
        .unwrap();
    }

    /// A bounded target's prompt must describe the TARGET's skills, not the
    /// caller's.
    ///
    /// The skill *tools* a bounded target gets are already scoped to it, but the
    /// prompt was still built from `self.workspace_dir` — the caller's — so the
    /// model was told about skills it had no tools for, and not told about the
    /// ones it did. Same tools-from-B / prompt-from-A split the independent path
    /// already guards against.
    #[tokio::test]
    async fn bounded_delegate_prompt_describes_target_skills_not_callers() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig,
        };

        let tmp = TempDir::new().unwrap();
        let caller_workspace = tmp.path().join("caller-ws");
        std::fs::create_dir_all(&caller_workspace).unwrap();

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
        config
            .risk_profiles
            .insert("target".to_string(), RiskProfileConfig::default());
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
                runtime_profile: "agentic".into(),
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

        // Each workspace gets a skill the other does not have.
        let target_workspace = config.agent_workspace_dir("target");
        std::fs::create_dir_all(&target_workspace).unwrap();
        write_skill(
            &caller_workspace,
            "callers-only-skill",
            "Belongs to the caller",
        );
        write_skill(
            &target_workspace,
            "targets-own-skill",
            "Belongs to the target",
        );

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            // The caller's own session workspace, which is what bounded
            // delegation used to build the prompt from.
            .with_workspace_dir(caller_workspace.clone())
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let provider = SystemPromptCapturingProvider::new();
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "do the thing",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        let prompt = provider
            .captured()
            .expect("the delegated turn must receive a system prompt");
        assert!(
            prompt.contains("targets-own-skill"),
            "a bounded target's prompt must describe its OWN skills; got: {prompt}"
        );
        assert!(
            !prompt.contains("callers-only-skill"),
            "a bounded target's prompt must not describe the CALLER's skills; got: {prompt}"
        );
    }

    /// The `skill_bundles` branch, which C-5 flagged separately: it resolved
    /// each bundle directory by joining it onto the workspace it was given, so
    /// a target's own bundle names were read out of the CALLER's workspace.
    ///
    /// Populating `sub_skills` short-circuits the resolver entirely, so this
    /// branch cannot join anything onto the wrong workspace - but the branch is
    /// distinct enough to be worth asserting rather than assuming.
    #[tokio::test]
    async fn bounded_delegate_prompt_resolves_skill_bundles_from_target_workspace() {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, RiskProfileConfig, RuntimeProfileConfig, SkillBundleConfig,
        };

        let tmp = TempDir::new().unwrap();
        let caller_workspace = tmp.path().join("caller-ws");
        std::fs::create_dir_all(&caller_workspace).unwrap();

        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.skill_bundles.insert(
            "shared".to_string(),
            SkillBundleConfig {
                directory: Some("bundled".to_string()),
                include: Vec::new(),
                exclude: Vec::new(),
            },
        );
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
        config
            .risk_profiles
            .insert("target".to_string(), RiskProfileConfig::default());
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
                runtime_profile: "agentic".into(),
                model_provider: "ollama.caller".into(),
                delegates: vec![DelegateTargetConfig::bounded("target")],
                skill_bundles: vec!["shared".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "target".to_string(),
            AliasedAgentConfig {
                risk_profile: "target".into(),
                runtime_profile: "agentic".into(),
                model_provider: "ollama.target".into(),
                skill_bundles: vec!["shared".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        let config = Arc::new(config);

        // The SAME bundle-relative directory exists in both workspaces, holding
        // a different skill in each. Only the join base tells them apart.
        let target_workspace = config.agent_workspace_dir("target");
        for (ws, skill) in [
            (&caller_workspace, "callers-bundled-skill"),
            (&target_workspace, "targets-bundled-skill"),
        ] {
            let dir = ws.join("bundled").join(skill);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {skill}\ndescription: Bundle skill {skill}\n---\n\nBody.\n"),
            )
            .unwrap();
        }

        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));
        let tool = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_skill_bundles(config.skill_bundles.clone())
            .with_workspace_dir(caller_workspace.clone())
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        let target_config = config.agents.get("target").expect("target exists").clone();
        let provider = SystemPromptCapturingProvider::new();
        let result = tool
            .execute_agentic(
                "target",
                &target_config,
                "ollama",
                "test-model",
                &provider,
                "do the thing",
                None,
            )
            .await
            .expect("bounded delegation runs");
        assert!(result.success, "delegation failed: {:?}", result.error);

        let prompt = provider
            .captured()
            .expect("the delegated turn must receive a system prompt");
        assert!(
            !prompt.contains("callers-bundled-skill"),
            "a bounded target's bundle must not resolve against the CALLER's \
             workspace; got: {prompt}"
        );
    }
}

#[cfg(test)]
mod tool_arc_ref_spec_tests {
    use super::*;
    use zeroclaw_api::tool::ToolSpec;

    struct ArcSchemaTool {
        schema: Arc<serde_json::Value>,
    }

    impl ::zeroclaw_api::attribution::Attributable for ArcSchemaTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            "arc-schema-tool"
        }
    }

    #[async_trait]
    impl Tool for ArcSchemaTool {
        fn name(&self) -> &str {
            "arc_schema_tool"
        }

        fn description(&self) -> &str {
            "test tool with Arc-shared schema"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            (*self.schema).clone()
        }

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name().to_string(),
                description: self.description().to_string(),
                parameters: Arc::clone(&self.schema),
                output: None,
                param_domains: std::collections::BTreeMap::new(),
            }
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[test]
    fn tool_arc_ref_forwards_spec_arc_identity() {
        let inner: Arc<dyn Tool> = Arc::new(ArcSchemaTool {
            schema: Arc::new(serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            })),
        });
        let inner_params = inner.spec().parameters;
        let wrapped = ToolArcRef::new(Arc::clone(&inner));

        assert!(
            Arc::ptr_eq(&wrapped.spec().parameters, &inner_params),
            "ToolArcRef must forward spec() so the inner Arc-shared schema \
             survives; the trait default deep-clones it every call"
        );
    }

    /// Trigger-owning tool standing in for `send_via` behind bounded
    /// delegation's `ToolArcRef` wrapper.
    struct TriggerOwningTool;

    impl ::zeroclaw_api::attribution::Attributable for TriggerOwningTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            "trigger-owning-tool"
        }
    }

    #[async_trait]
    impl Tool for TriggerOwningTool {
        fn name(&self) -> &str {
            "trigger_owning_tool"
        }

        fn description(&self) -> &str {
            "test tool with invocation triggers"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }

        fn invocation_triggers(&self) -> Vec<String> {
            vec!["send this to".into(), "as a voice message".into()]
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[test]
    fn tool_arc_ref_forwards_invocation_triggers() {
        // Regression: bounded delegation re-wraps every admitted parent tool
        // in `ToolArcRef` (see the sub-tool assembly in `execute`). Without
        // explicit forwarding the trait default returns an empty vocabulary,
        // silently erasing a trigger-owning tool's metadata for any consumer
        // scanning a delegate's assembled tools.
        let inner: Arc<dyn Tool> = Arc::new(TriggerOwningTool);
        let wrapped = ToolArcRef::new(Arc::clone(&inner));

        assert_eq!(
            wrapped.invocation_triggers(),
            inner.invocation_triggers(),
            "ToolArcRef must forward invocation_triggers(); the trait \
             default erases the inner tool's vocabulary"
        );
        assert!(
            wrapped
                .invocation_triggers()
                .iter()
                .any(|t| t == "send this to"),
            "wrapped trigger vocabulary must survive bounded delegation"
        );
    }
}

/// A bounded target's rebuilt `send_via` must resolve peer-group authority from
/// the TARGET's groups, not the caller's.
///
/// The alias is captured in a closure over the peer-group resolver rather than
/// stored in a field, so it is invisible to any check that inspects the tool's
/// struct: only executing the reused instance can tell the two apart. These
/// tests execute it, through the real bounded rebuild, with caller and target
/// in DIFFERENT groups.
#[cfg(test)]
mod bounded_send_via_peer_group_authority {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use ::zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use ::zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};
    use zeroclaw_config::schema::{Config, DelegateExecutionMode, DelegateTargetConfig};
    use zeroclaw_providers::{ChatRequest, ChatResponse, ToolCall};

    const CHANNEL: &str = "telegram.default";
    /// Peer group the CALLER belongs to and the target does not. Addressed by
    /// group name, which is how `send_via` resolves a target: a bare username
    /// is read as a channel alias, not as a peer.
    const CALLER_ONLY_GROUP: &str = "calleronlygroup";
    /// Peer group the TARGET belongs to and the caller does not.
    const TARGET_ONLY_GROUP: &str = "targetonlygroup";

    struct StubChannel {
        sent: Arc<parking_lot::RwLock<Vec<String>>>,
    }

    impl Attributable for StubChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Webhook)
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl Channel for StubChannel {
        fn name(&self) -> &str {
            CHANNEL
        }
        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.write().push(format!("{message:?}"));
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Calls `send_via` once against `target`, then reports the tool result it
    /// observed on the following turn.
    struct SendViaProbeProvider {
        target: String,
        captured: Arc<parking_lot::RwLock<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for SendViaProbeProvider {
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
            for message in request.messages.iter().filter(|m| m.role == "tool") {
                self.captured.write().push(message.content.clone());
            }
            if self.captured.read().is_empty() {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "send_via".to_string(),
                        arguments: format!("{{\"target\":\"{}\",\"body\":\"probe\"}}", self.target),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }

    impl Attributable for SendViaProbeProvider {
        fn role(&self) -> Role {
            Role::Provider(::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ))
        }
        fn alias(&self) -> &str {
            "probe"
        }
    }

    fn peer_group(agent: &str, peer: &str) -> PeerGroupConfig {
        PeerGroupConfig {
            channel: "telegram".into(),
            agents: vec![AgentAlias::new(agent)],
            external_peers: vec![PeerUsername::new(peer)],
            ..PeerGroupConfig::default()
        }
    }

    /// Caller and target sit in disjoint peer groups over the same channel.
    fn disjoint_peer_group_config() -> Config {
        let mut config = Config::default();
        config.risk_profiles.insert(
            "shared".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Full,
                allowed_tools: vec!["send_via".to_string()],
                delegation_policy: zeroclaw_config::autonomy::DelegationPolicy {
                    mode: zeroclaw_config::autonomy::DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 3,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                enabled: true,
                risk_profile: "shared".into(),
                runtime_profile: "agentic".into(),
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
                enabled: true,
                risk_profile: "shared".into(),
                runtime_profile: "agentic".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.peer_groups.insert(
            CALLER_ONLY_GROUP.to_string(),
            peer_group("caller", "@callerpeer"),
        );
        config.peer_groups.insert(
            TARGET_ONLY_GROUP.to_string(),
            peer_group("target", "@targetpeer"),
        );
        config
    }

    /// Runs a bounded delegation whose target calls `send_via` against `peer`,
    /// and returns the tool results the delegated turn observed.
    async fn delegate_and_send_to(group: &str) -> (Vec<String>, usize) {
        let config = Arc::new(disjoint_peer_group_config());
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));

        let sent = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let handle: crate::tools::PerToolChannelHandle =
            Arc::new(parking_lot::RwLock::new(HashMap::new()));
        handle.write().insert(
            CHANNEL.to_string(),
            Arc::new(StubChannel {
                sent: Arc::clone(&sent),
            }) as Arc<dyn Channel>,
        );

        // The caller's own instance, which is what a bounded target would reuse
        // if the rebuild were skipped. Its resolver is bound to "caller".
        let caller_send_via = crate::tools::send_via_tool(
            Arc::clone(&caller_policy),
            &config,
            None,
            "caller",
            Arc::clone(&handle),
        );

        let handles = crate::tools::DelegateChannelHandles {
            ask_user: Some(Arc::clone(&handle)),
            ..Default::default()
        };

        let delegate = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_channel_handles(handles)
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_send_via])));

        let captured = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let provider = SendViaProbeProvider {
            target: group.to_string(),
            captured: Arc::clone(&captured),
        };
        let target_cfg = config.agents.get("target").expect("target configured");

        let _ = delegate
            .execute_agentic(
                "target",
                target_cfg,
                "test",
                "test-model",
                &provider,
                "reach the peer",
                None,
            )
            .await;

        let out = captured.read().clone();
        let delivered = sent.read().len();
        (out, delivered)
    }

    #[tokio::test]
    async fn bounded_target_cannot_reach_a_peer_only_the_caller_is_grouped_with() {
        // MUST FAIL if the target reuses the caller's `send_via` instance, whose
        // resolver closes over the CALLER's alias: the caller-only peer would
        // then resolve and the send would be authorized.
        let (results, delivered) = delegate_and_send_to(CALLER_ONLY_GROUP).await;
        let joined = results.join("\n");

        assert!(
            !joined.is_empty(),
            "the delegated turn never produced a `send_via` result"
        );
        assert!(
            joined.contains("rejected"),
            "reaching the caller-only group was not rejected; got {joined}"
        );
        assert_eq!(
            delivered, 0,
            "the caller-only group was rejected yet a message still reached the channel; got {joined}"
        );
    }

    #[tokio::test]
    async fn bounded_target_can_reach_a_peer_from_its_own_group() {
        // Positive half. Without it, a rebuild that simply broke `send_via`
        // would satisfy the rejection above while proving nothing: the point is
        // that authority MOVED to the target, not that it vanished.
        let (results, delivered) = delegate_and_send_to(TARGET_ONLY_GROUP).await;
        let joined = results.join("\n");

        assert!(
            !joined.is_empty(),
            "the delegated turn never produced a `send_via` result"
        );
        assert!(
            delivered >= 1,
            "the target own group delivered nothing: authority did not MOVE to the target, it only disappeared; got {joined}"
        );
    }
}

/// An MCP tool name prefix is not the identity of a grant.
///
/// A bounded target may reuse the caller's MCP tools for servers the TARGET was
/// granted. Admission is decided by matching the tool name against
/// `format!("{server}__")` for each granted server. That mapping is not
/// injective: nothing forbids a server name from containing `__` itself, so a
/// grant for `foo` also admits every tool of a DIFFERENT server named
/// `foo__admin`, which the caller's registry routes elsewhere entirely.
///
/// The two tools are real MCP wrappers over registries that genuinely route
/// them - one per server - so the ambiguity is not asserted from a name but
/// present in the fixture: `foo__admin__wipe` is routed by its own registry to
/// `foo__admin`, a server the target was never granted.
#[cfg(test)]
mod bounded_mcp_prefix_is_not_grant_identity {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use zeroclaw_config::schema::{
        Config, DelegateExecutionMode, DelegateTargetConfig, McpBundleConfig, McpServerConfig,
    };
    use zeroclaw_providers::{ChatRequest, ChatResponse};

    /// Server both caller and target hold.
    const GRANTED_SERVER: &str = "foo";
    /// Server ONLY the caller holds. Its name contains the separator, which is
    /// what makes the prefix rule ambiguous.
    const UNGRANTED_SERVER: &str = "foo__admin";

    /// Tool of the granted server. Must survive.
    const GRANTED_TOOL: &str = "foo__lookup";
    /// Tool of the ungranted server. Its name also begins with `foo__`, so a
    /// prefix test cannot tell it apart from a tool of `foo`.
    const UNGRANTED_TOOL: &str = "foo__admin__wipe";

    /// Records the tool names the delegated turn was offered.
    struct OfferedToolNamesProvider {
        seen: Arc<parking_lot::RwLock<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for OfferedToolNamesProvider {
        /// Without this the loop describes the tools in the prompt instead of
        /// populating `request.tools`, and the observation below sees nothing.
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
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            if let Some(tools) = request.tools {
                let mut seen = self.seen.write();
                for tool in tools {
                    seen.push(tool.name.clone());
                }
            }
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for OfferedToolNamesProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "offered"
        }
    }

    /// The caller is granted BOTH servers; the target only `foo`.
    fn split_grant_config() -> Config {
        let mut config = Config::default();
        config.mcp.servers.push(McpServerConfig {
            name: GRANTED_SERVER.to_string(),
            ..McpServerConfig::default()
        });
        config.mcp.servers.push(McpServerConfig {
            name: UNGRANTED_SERVER.to_string(),
            ..McpServerConfig::default()
        });
        config.mcp_bundles.insert(
            "caller_bundle".to_string(),
            McpBundleConfig {
                servers: vec![GRANTED_SERVER.to_string(), UNGRANTED_SERVER.to_string()],
                ..McpBundleConfig::default()
            },
        );
        config.mcp_bundles.insert(
            "target_bundle".to_string(),
            McpBundleConfig {
                servers: vec![GRANTED_SERVER.to_string()],
                ..McpBundleConfig::default()
            },
        );

        config.risk_profiles.insert(
            "shared".to_string(),
            RiskProfileConfig {
                level: AutonomyLevel::Full,
                // Both names are policy-allowed on both sides, so the ONLY gate
                // left to discriminate them is the MCP server-prefix rule.
                allowed_tools: vec![GRANTED_TOOL.to_string(), UNGRANTED_TOOL.to_string()],
                delegation_policy: zeroclaw_config::autonomy::DelegationPolicy {
                    mode: zeroclaw_config::autonomy::DelegationMode::Allow,
                },
                ..RiskProfileConfig::default()
            },
        );
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 2,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                enabled: true,
                risk_profile: "shared".into(),
                runtime_profile: "agentic".into(),
                mcp_bundles: vec!["caller_bundle".to_string()],
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
                enabled: true,
                risk_profile: "shared".into(),
                runtime_profile: "agentic".into(),
                mcp_bundles: vec!["target_bundle".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    /// Tool names the bounded target was offered, plus the delegation outcome
    /// so an empty set can be distinguished from a delegation that never ran.
    async fn offered_to_bounded_target() -> (Vec<String>, String) {
        let config = Arc::new(split_grant_config());
        // Fixture guard: if the target holds no granted server the prefix rule
        // has nothing to match and every assertion below would be about the
        // fixture, not about admission.
        assert_eq!(
            config
                .mcp_servers_for_agent("target")
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>(),
            vec![GRANTED_SERVER.to_string()],
            "fixture: the target must hold exactly the granted server"
        );
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));

        // One registry per server, each routing its own tool, so the owning
        // server is a fact of the fixture rather than a reading of the name.
        let wrapper = |server: &str, tool: &str| -> Arc<dyn Tool> {
            let registry = Arc::new(
                zeroclaw_tools::mcp_client::McpRegistry::for_test_with_scripted_tool(
                    server,
                    tool,
                    serde_json::json!({"content": [{"type": "text", "text": "ok"}]}),
                ),
            );
            let def = zeroclaw_tools::mcp_protocol::McpToolDef {
                name: tool.to_string(),
                description: Some("scripted".to_string()),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            };
            Arc::new(crate::tools::McpToolWrapper::new(
                format!("{server}__{tool}"),
                def,
                registry,
                Arc::clone(&caller_policy),
            ))
        };
        let parent_tools: Vec<Arc<dyn Tool>> = vec![
            wrapper(GRANTED_SERVER, "lookup"),
            wrapper(UNGRANTED_SERVER, "wipe"),
        ];

        let delegate = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(parent_tools)));

        let seen = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let provider = OfferedToolNamesProvider {
            seen: Arc::clone(&seen),
        };
        let target_cfg = config.agents.get("target").expect("target configured");

        let outcome = delegate
            .execute_agentic(
                "target",
                target_cfg,
                "test",
                "test-model",
                &provider,
                "use what you have",
                None,
            )
            .await;

        let out = seen.read().clone();
        (out, format!("{outcome:?}"))
    }

    #[tokio::test]
    async fn a_tool_of_an_ungranted_server_sharing_the_granted_prefix_is_not_admitted() {
        // MUST FAIL while admission is `tool.name().starts_with("foo__")`:
        // `foo__admin__wipe` satisfies that test even though `foo__admin` is a
        // server the target was never granted.
        let (offered, outcome) = offered_to_bounded_target().await;

        assert!(
            !offered.is_empty(),
            "the delegated turn was offered no tools at all, so this assertion would hold \n             vacuously; outcome {outcome}"
        );
        assert!(
            !offered.iter().any(|name| name == UNGRANTED_TOOL),
            "a tool of the ungranted `{UNGRANTED_SERVER}` server was admitted because its name \
             starts with the granted server prefix; offered {offered:?}"
        );
    }

    #[tokio::test]
    async fn a_tool_of_the_granted_server_is_still_admitted() {
        // Positive half. Resolving admission against server identity instead of
        // the name prefix must not cost the target the tools it IS granted; a
        // rule that denied both would satisfy the assertion above and break the
        // feature.
        let (offered, outcome) = offered_to_bounded_target().await;

        assert!(
            offered.iter().any(|name| name == GRANTED_TOOL),
            "the target lost a tool of the `{GRANTED_SERVER}` server it WAS granted; \
             offered {offered:?}; outcome {outcome}"
        );
    }
}

/// A reused MCP wrapper materializes attachments into the CALLER's workspace.
///
/// The wrapper stores a `SecurityPolicy` whose only use is supplying the
/// directory that embedded `resource` blobs are written under. A bounded target
/// granted the same server reuses the caller's instance, so the policy - and
/// therefore the destination directory - is the caller's, even though the work
/// is the target's. Nothing about the instance's NAME or its inner struct shows
/// this: the leak is in a wrapper field, reached only on the execution path,
/// and only when the server actually returns a blob.
#[cfg(test)]
mod bounded_mcp_wrapper_materializes_into_target_workspace {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;
    use zeroclaw_config::schema::{
        Config, DelegateExecutionMode, DelegateTargetConfig, McpBundleConfig, McpServerConfig,
    };
    use zeroclaw_providers::{ChatRequest, ChatResponse, ToolCall};

    const SERVER: &str = "srv";
    const TOOL: &str = "fetch";
    const PREFIXED: &str = "srv__fetch";

    /// A `tools/call` result carrying one embedded resource blob. Without a
    /// blob the formatter returns the payload untouched and never writes
    /// anything, so this is the branch that has to be exercised.
    fn blob_result() -> serde_json::Value {
        serde_json::json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///attachment.txt",
                    "mimeType": "text/plain",
                    "blob": "aGVsbG8gZnJvbSB0aGUgc2VydmVy"
                }
            }]
        })
    }

    /// Emits one call to the reused MCP tool, then finishes.
    struct CallMcpToolProvider {
        called: Arc<parking_lot::RwLock<bool>>,
    }

    #[async_trait]
    impl ModelProvider for CallMcpToolProvider {
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
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            let offered = request
                .tools
                .is_some_and(|tools| tools.iter().any(|t| t.name == PREFIXED));
            if offered && !*self.called.read() {
                *self.called.write() = true;
                return Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: PREFIXED.to_string(),
                        arguments: "{}".to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for CallMcpToolProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "mcpcaller"
        }
    }

    fn cross_profile_config(caller_ws: &std::path::Path, target_ws: &std::path::Path) -> Config {
        let mut config = Config::default();
        config.mcp.servers.push(McpServerConfig {
            name: SERVER.to_string(),
            ..McpServerConfig::default()
        });
        let bundle = McpBundleConfig {
            servers: vec![SERVER.to_string()],
            ..McpBundleConfig::default()
        };
        config.mcp_bundles.insert("shared_srv".to_string(), bundle);

        let profile = || RiskProfileConfig {
            level: AutonomyLevel::Full,
            allowed_tools: vec![PREFIXED.to_string()],
            delegation_policy: zeroclaw_config::autonomy::DelegationPolicy {
                mode: zeroclaw_config::autonomy::DelegationMode::Allow,
            },
            ..RiskProfileConfig::default()
        };
        // Distinct profile names: same-profile delegation deliberately keeps the
        // caller's session workspace, which would make the destination the
        // caller's for an entirely different and legitimate reason.
        config
            .risk_profiles
            .insert("caller_p".to_string(), profile());
        config
            .risk_profiles
            .insert("target_p".to_string(), profile());
        config.runtime_profiles.insert(
            "agentic".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 3,
                ..RuntimeProfileConfig::default()
            },
        );

        let ws = |p: &std::path::Path| zeroclaw_config::multi_agent::AgentWorkspaceConfig {
            path: Some(p.to_path_buf()),
            ..Default::default()
        };

        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                enabled: true,
                risk_profile: "caller_p".into(),
                runtime_profile: "agentic".into(),
                workspace: ws(caller_ws),
                mcp_bundles: vec!["shared_srv".to_string()],
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
                enabled: true,
                risk_profile: "target_p".into(),
                runtime_profile: "agentic".into(),
                workspace: ws(target_ws),
                mcp_bundles: vec!["shared_srv".to_string()],
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    /// Number of files under `<workspace>/uploads`.
    fn uploads_count(workspace: &std::path::Path) -> usize {
        std::fs::read_dir(workspace.join("uploads"))
            .map(|entries| entries.flatten().count())
            .unwrap_or(0)
    }

    struct Outcome {
        caller_uploads: usize,
        target_uploads: usize,
        tool_called: bool,
        result: String,
    }

    async fn delegate_and_call_shared_mcp_tool(root: &std::path::Path) -> Outcome {
        let caller_ws = root.join("caller-workspace");
        let target_ws = root.join("target-workspace");
        std::fs::create_dir_all(&caller_ws).expect("caller workspace");
        std::fs::create_dir_all(&target_ws).expect("target workspace");

        let config = Arc::new(cross_profile_config(&caller_ws, &target_ws));
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));

        // The caller's own wrapper, exactly as its registry would hold it: bound
        // to the caller's policy, which is where the destination comes from.
        let registry = Arc::new(crate::tools::McpRegistry::for_test_with_scripted_tool(
            SERVER,
            TOOL,
            blob_result(),
        ));
        let def = zeroclaw_tools::mcp_protocol::McpToolDef {
            name: TOOL.to_string(),
            description: Some("scripted".to_string()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        let caller_wrapper: Arc<dyn Tool> = Arc::new(crate::tools::McpToolWrapper::new(
            PREFIXED.to_string(),
            def,
            Arc::clone(&registry),
            Arc::clone(&caller_policy),
        ));

        let delegate = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_wrapper])));

        let called = Arc::new(parking_lot::RwLock::new(false));
        let provider = CallMcpToolProvider {
            called: Arc::clone(&called),
        };
        let target_cfg = config.agents.get("target").expect("target configured");

        let result = delegate
            .execute_agentic(
                "target",
                target_cfg,
                "test",
                "test-model",
                &provider,
                "fetch the attachment",
                None,
            )
            .await;

        Outcome {
            caller_uploads: uploads_count(&caller_ws),
            target_uploads: uploads_count(&target_ws),
            tool_called: *called.read(),
            result: format!("{result:?}"),
        }
    }

    #[tokio::test]
    async fn a_reused_mcp_wrapper_writes_attachments_into_the_targets_workspace() {
        let tmp = TempDir::new().expect("temp root");
        let outcome = delegate_and_call_shared_mcp_tool(tmp.path()).await;

        // Guard first: if the tool was never offered or never called, both
        // counts are zero and every assertion below holds for the wrong reason.
        assert!(
            outcome.tool_called,
            "the delegated turn never called the shared MCP tool, so nothing was materialized; \
             result {}",
            outcome.result
        );

        assert_eq!(
            outcome.target_uploads, 1,
            "the attachment did not land in the TARGET workspace (target={}, caller={}); \
             result {}",
            outcome.target_uploads, outcome.caller_uploads, outcome.result
        );
        assert_eq!(
            outcome.caller_uploads, 0,
            "the attachment landed in the CALLER workspace: the reused wrapper is still \
             materializing against the policy it captured (target={}, caller={}); result {}",
            outcome.target_uploads, outcome.caller_uploads, outcome.result
        );
    }
}

/// A reused rate-limited tool meters the target against the CALLER's budget.
///
/// The registered instance is `RateLimitedTool<WebSearchTool>`, and it is the
/// WRAPPER that holds a `SecurityPolicy`: the inner search tool has no such
/// field at all. Reading the inner struct therefore says the instance is free
/// of caller capture, which is exactly the category error that let this one
/// through - the tool is the registered instance, wrappers included.
///
/// Both cases are decided BEFORE the inner tool runs, so neither reaches the
/// network: `RateLimitedTool::execute` reserves first and returns early on
/// refusal, and the inner tool is configured with a provider whose API key is
/// absent, which it rejects before issuing any request. The two failures are
/// distinguishable by message, which is what makes the pair discriminating in
/// both directions.
///
/// Declared limit: this exercises WHICH policy's ceiling is consulted, not
/// accumulation across several calls. Accumulation is unreachable without the
/// network here, because the reservation is only committed when the inner tool
/// SUCCEEDS - a failing inner call leaves the budget untouched, so repeated
/// calls never add up. The tracker cannot be pre-charged from the test either:
/// its bucket is keyed by the tool loop's own thread id, so a reservation made
/// outside the delegated loop lands in a different bucket.
#[cfg(test)]
mod bounded_rate_limit_is_the_targets_own {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use zeroclaw_config::schema::{Config, DelegateExecutionMode, DelegateTargetConfig};
    use zeroclaw_providers::{ChatRequest, ChatResponse, ToolCall};

    const TOOL: &str = "web_search_tool";
    /// Raised by the wrapper when the reservation is refused.
    const RATE_LIMIT_MARKER: &str = "Rate limit exceeded";
    /// Substring of every error the INNER tool raises while resolving its
    /// provider credential - it never gets as far as a request. Seeing it means
    /// the wrapper ADMITTED the call, which is the fact under test; the precise
    /// wording depends on whether a config file happens to exist and is
    /// deliberately not pinned.
    const INNER_REACHED_MARKER: &str = "Brave API key";

    /// Emits one call to the reused tool, then reports the tool result back.
    struct CallSearchProvider {
        captured: Arc<parking_lot::RwLock<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for CallSearchProvider {
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
            Ok("unused".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            for message in request.messages.iter().filter(|m| m.role == "tool") {
                self.captured.write().push(message.content.clone());
            }
            if self.captured.read().is_empty() {
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: TOOL.to_string(),
                        arguments: "{\"query\":\"anything\"}".to_string(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                })
            } else {
                Ok(ChatResponse {
                    text: Some("done".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for CallSearchProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "search"
        }
    }

    fn budget_config(caller_budget: u32, target_budget: u32) -> Config {
        let mut config = Config::default();
        // Pin a credential-requiring provider with NO key. Both the caller's
        // instance and the rebuilt target instance are built from this config,
        // so both refuse before issuing any request. Leaving the default
        // provider here makes the rebuilt instance perform a REAL search the
        // moment the rebuild starts working.
        config.web_search.enabled = true;
        config.web_search.search_provider = "brave".to_string();
        config.web_search.brave_api_key = None;
        let profile = || RiskProfileConfig {
            level: AutonomyLevel::Full,
            allowed_tools: vec!["delegate".to_string(), TOOL.to_string()],
            delegation_policy: zeroclaw_config::autonomy::DelegationPolicy {
                mode: zeroclaw_config::autonomy::DelegationMode::Allow,
            },
            ..RiskProfileConfig::default()
        };
        // Distinct risk profiles on purpose: a shared one would also drag the
        // caller's session workspace onto the target, mixing two separate
        // concerns into one fixture. The action budget itself lives on the
        // RUNTIME profile, so that is where the two sides have to differ.
        config
            .risk_profiles
            .insert("caller_p".to_string(), profile());
        config
            .risk_profiles
            .insert("target_p".to_string(), profile());
        let runtime = |budget: u32| RuntimeProfileConfig {
            agentic: true,
            max_tool_iterations: 3,
            max_actions_per_hour: budget,
            ..RuntimeProfileConfig::default()
        };
        config
            .runtime_profiles
            .insert("caller_rt".to_string(), runtime(caller_budget));
        config
            .runtime_profiles
            .insert("target_rt".to_string(), runtime(target_budget));
        config.agents.insert(
            "caller".to_string(),
            AliasedAgentConfig {
                enabled: true,
                risk_profile: "caller_p".into(),
                runtime_profile: "caller_rt".into(),
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
                enabled: true,
                risk_profile: "target_p".into(),
                runtime_profile: "target_rt".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    /// Delegates with the two budgets and returns what the tool reported.
    async fn tool_result_with_budgets(caller_budget: u32, target_budget: u32) -> String {
        let config = Arc::new(budget_config(caller_budget, target_budget));
        let caller_policy =
            Arc::new(SecurityPolicy::for_agent(&config, "caller").expect("caller policy resolves"));

        // The caller's registered instance, built through the SAME production
        // factory the rebuild uses, so the two sides cannot differ in provider
        // or wrapper shape - only in the policy each was built with.
        let caller_instance = crate::tools::web_search_tool(Arc::clone(&caller_policy), &config)
            .expect("web_search is enabled in this fixture");

        let delegate = DelegateTool::new(config.agents.clone(), None, Arc::clone(&caller_policy))
            .with_root_config(Arc::clone(&config))
            .with_caller_alias("caller")
            .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
            .with_risk_profiles(config.risk_profiles.clone())
            .with_runtime_profiles(config.runtime_profiles.clone())
            .with_parent_tools(Arc::new(RwLock::new(vec![caller_instance])));

        let captured = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let provider = CallSearchProvider {
            captured: Arc::clone(&captured),
        };
        let target_cfg = config.agents.get("target").expect("target configured");

        let outcome = delegate
            .execute_agentic(
                "target",
                target_cfg,
                "test",
                "test-model",
                &provider,
                "search for something",
                None,
            )
            .await;

        let joined = captured.read().join("\n");
        format!("{joined} || outcome {outcome:?}")
    }

    #[tokio::test]
    async fn an_exhausted_target_budget_stops_the_call_even_when_the_caller_has_room() {
        // Caller could act 100 more times; the target may not act at all.
        //
        // MUST FAIL while the target reuses the caller's wrapped instance: the
        // reservation is then made against the caller's budget, succeeds, and
        // the inner tool runs - which the marker below makes visible.
        let result = tool_result_with_budgets(100, 0).await;

        assert!(
            result.contains(RATE_LIMIT_MARKER),
            "the target acted on the caller's budget instead of its own exhausted one; {result}"
        );
        assert!(
            !result.contains(INNER_REACHED_MARKER),
            "the inner search tool was reached, so the wrapper admitted a call the target had \
             no budget for; {result}"
        );
    }

    #[tokio::test]
    async fn a_target_with_budget_is_not_stopped_by_an_exhausted_caller() {
        // The mirror image, and the half that keeps the assertion above from
        // being satisfiable by a wrapper that simply denies everything: the
        // caller is exhausted, the target is not, and the call must proceed.
        //
        // MUST FAIL while the target reuses the caller's wrapped instance: the
        // caller's zero budget would refuse a call the target is entitled to.
        let result = tool_result_with_budgets(0, 100).await;

        assert!(
            !result.contains(RATE_LIMIT_MARKER),
            "the target was refused on the CALLER's exhausted budget; {result}"
        );
        assert!(
            result.contains(INNER_REACHED_MARKER),
            "the call never reached the inner tool, so it is not clear the wrapper admitted \
             it at all; {result}"
        );
    }
}
