use crate::agent::loop_::{LoopKnobs, TOOL_LOOP_SESSION_KEY, run_tool_call_loop};
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, DelegateToolConfig, ModelProviderConfig, ResolvedRuntime,
    RiskProfileConfig, RuntimeProfileConfig, SkillBundleConfig,
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

/// Tool that delegates a subtask to a named agent with a different
/// model_provider/model configuration. Enables multi-agent workflows where
/// a primary agent can hand off specialized work (research, coding,
/// summarization) to purpose-built sub-agents.
///
/// Supports three execution modes:
/// - **Synchronous** (default): blocks until the sub-agent completes.
/// - **Background** (`background: true`): spawns the sub-agent in a tokio
///   task and returns a `task_id` immediately.
/// - **Parallel** (`parallel: [...]`): runs multiple agents concurrently
///   and returns all results.
///
/// Background results are persisted to `workspace/delegate_results/{task_id}.json`
/// and can be retrieved via `action: "check_result"`.
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
    /// Optional handle to the loaded root config used to resolve a
    /// per-target `SecurityPolicy` at delegate time. When set, every
    /// delegation requires the target agent to share the caller's risk
    /// profile and inherits the caller's `PerSenderTracker` so action
    /// / cost budgets are shared between caller and delegated runs.
    /// When unset (legacy unit-test constructors), DelegateTool falls
    /// back to using `self.security` for the spawned inner DelegateTool.
    root_config: Option<Arc<Config>>,
    /// Alias of the agent that owns this DelegateTool. Excluded from the
    /// advertised roster so an agent is never offered itself as a
    /// delegation target. Empty when unset (legacy unit-test constructors).
    caller_alias: String,
}

impl DelegateTool {
    /// Canonical tool name. Referenced by `REENTRANT_AGENT_TOOLS` so a
    /// rename cannot desync the two.
    pub const NAME: &'static str = "delegate";

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

    /// Resolve a target sub-agent's workspace dir for identity-file
    /// loading. Delegates to `Config::agent_workspace_dir` so the
    /// per-agent path lives in one place; returns `None` when no
    /// `root_config` is attached (legacy unit-test constructors), which
    /// callers treat as "no identity files to load".
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

    /// Attach the loaded root config so DelegateTool can resolve a
    /// per-target `SecurityPolicy` at delegate time, require the
    /// target to share the caller's risk profile, and share the
    /// caller's `PerSenderTracker` with the delegated run.
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

    /// Resolve the target's `SecurityPolicy` for delegation.
    ///
    /// Refuses when the caller's `delegation_policy` forbids delegation
    /// or the target is outside the caller's `reachable_delegate_targets`
    /// set. Same-profile targets inherit the caller's session workspace
    /// boundary (issue #7263); cross-profile explicit delegates keep
    /// their own workspace and must pass `ensure_no_escalation_beyond`
    /// the caller so a listed delegate cannot widen privilege. Both
    /// share the caller's action/cost tracker. Falls back to the
    /// caller's policy when no `root_config` is attached (legacy
    /// unit-test constructors).
    fn policy_for_target(&self, target_alias: &str) -> anyhow::Result<Arc<SecurityPolicy>> {
        let Some(config) = self.root_config.as_ref() else {
            return Ok(Arc::clone(&self.security));
        };
        if !self.security.delegation_policy.permits() {
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
                "delegation is forbidden by the caller's delegation_policy; set \
                 [risk_profiles.{}].delegation_policy mode = \"allow\"",
                self.security.risk_profile_name
            )));
        }

        if !config
            .reachable_delegate_targets(&self.caller_alias)
            .iter()
            .any(|name| name == target_alias)
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target_agent": target_alias,
                        "caller_alias": self.caller_alias,
                    })),
                "delegate refused: target not in caller's reachable set"
            );
            return Err(anyhow::Error::msg(format!(
                "delegate target {target_alias:?} is not reachable from {:?}; \
                 add it to [agents.{}].delegates or share a risk profile with \
                 delegate_same_risk_profile enabled",
                self.caller_alias, self.caller_alias
            )));
        }

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

        target_policy.tracker = self.security.tracker.clone();

        if self.security.risk_profile_name == target_policy.risk_profile_name {
            target_policy.workspace_dir = self.security.workspace_dir.clone();
        } else if let Err(violation) = target_policy.ensure_no_escalation_beyond(&self.security) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target_agent": target_alias,
                        "caller_alias": self.caller_alias,
                        "caller_risk_profile": self.security.risk_profile_name,
                        "target_risk_profile": target_policy.risk_profile_name,
                        "violation": format!("{violation:?}"),
                    })),
                "delegate refused: cross-profile target would escalate beyond caller"
            );
            return Err(anyhow::Error::msg(format!(
                "delegate target {target_alias:?} (risk profile {:?}) would escalate \
                 beyond the caller (risk profile {:?}): {violation:?}",
                target_policy.risk_profile_name, self.security.risk_profile_name
            )));
        }

        Ok(Arc::new(target_policy))
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

    /// Resolve `model_provider` ("type.alias") → (provider_type, credential, model, temperature).
    fn resolve_brain(&self, model_provider: &str) -> (String, Option<String>, String, Option<f64>) {
        if let Some((type_key, alias_key)) = model_provider.split_once('.')
            && let Some(alias_map) = self.providers_models.get(type_key)
            && let Some(cfg) = alias_map.get(alias_key)
        {
            return (
                type_key.to_string(),
                cfg.api_key
                    .clone()
                    .or_else(|| self.global_credential.clone()),
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

    /// Resolve the runtime-profile knobs the delegate sub-loop consumes.
    ///
    /// Production DelegateTool instances carry `root_config`, so use the
    /// canonical config resolver there. The fallback only serves legacy unit
    /// constructors that build DelegateTool from raw maps without a full Config.
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

    /// Materialize the tool gate from the named risk profile (authorization).
    ///
    /// Returns `None` when the risk profile is unnamed or not configured.
    /// `allowed_tools = Some(vec![])` means "deny all" and is preserved as
    /// `Some(empty)` so the caller can distinguish it from "no risk profile."
    ///
    /// The resulting `SecurityPolicy` only carries the tool authorization
    /// fields (`allowed_tools` and `excluded_tools`). Callers in
    /// `execute_agentic` must use [`Self::delegate_admits_with_mcp`] — not the
    /// raw `SecurityPolicy::is_tool_allowed` — to filter `parent_tools`,
    /// because the delegate path applies the MCP `<server>__<tool>`
    /// auto-admit exception described on `RiskProfileConfig::allowed_tools`.
    /// The exception is intentionally scoped to the risk-profile gate; it
    /// does not apply to caller-supplied per-run allow-lists (cron jobs and
    /// other narrowers) — see PR #7547 review.
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

    /// MCP-aware admission check used to filter `parent_tools` in
    /// `execute_agentic`.
    ///
    /// Same contract as `SecurityPolicy::is_tool_allowed`, with one
    /// addition that the delegate path needs: when the risk profile's
    /// `allowed_tools` is `Some(non-empty)`, any name containing `__`
    /// (the `<server>__<tool>` MCP wrapper convention) is auto-admitted
    /// even if it is not explicitly listed in `allowed_tools`. The
    /// `excluded_tools` deny-list always applies last, so destructive
    /// MCP capabilities like `filesystem__write_file` can be blocked
    /// individually.
    ///
    /// This auto-admit applies only to the risk-profile gate. Callers
    /// that need a per-run narrowing (cron jobs, narrowed delegate
    /// invocations) intersect their own allow-list against this result
    /// with a strict `list.contains(name)` check — see
    /// `ToolAccessPolicy::is_tool_allowed` and PR #7547.
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

    /// Persist a background result atomically: write to a sibling temp file then
    /// rename onto the final path, so a concurrent reader never observes a
    /// half-written (or zero-length) JSON document.
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
         Supports background execution (returns a task_id immediately) and parallel execution \
         (runs multiple agents concurrently). Use action='check_result' with a task_id to \
         retrieve background results."
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
                    "enum": ["delegate", "check_result", "list_results", "cancel_task"],
                    "description": "Action to perform. Default: 'delegate'. Use 'check_result' to \
                                    retrieve a background task result, 'list_results' to list all \
                                    background tasks, 'cancel_task' to cancel a running background task.",
                    "default": "delegate"
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
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("delegate");

        match action {
            "check_result" => return self.handle_check_result(&args).await,
            "list_results" => return self.handle_list_results().await,
            "cancel_task" => return self.handle_cancel_task(&args).await,
            "delegate" => {} // fall through to delegation logic
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown action '{other}'. Use delegate/check_result/list_results/cancel_task."
                    )),
                });
            }
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
                output: String::new(),
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
                output: String::new(),
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
                    output: String::new(),
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
                output: String::new(),
                error: Some(format!(
                    "Delegation depth limit reached ({depth}/{max}). \
                     Cannot delegate further to prevent infinite loops.",
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
                output: String::new(),
                error: Some(error),
            });
        }

        if let Err(e) = self.policy_for_target(agent_name) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("{e:#}")),
            });
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
                    output: String::new(),
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
                .execute_agentic(
                    agent_name,
                    agent_config,
                    &provider_type,
                    &model,
                    &*model_provider,
                    &full_prompt,
                    temperature,
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
                    output: String::new(),
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
                    output: format!("[Agent '{agent_name}' ({provider_type}/{model})]\n{rendered}",),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
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
                    output: String::new(),
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
                output: String::new(),
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
                output: String::new(),
                error: Some(error),
            });
        }

        let target_policy = match self.policy_for_target(agent_name) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{e:#}")),
                });
            }
        };

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

        let agents = Arc::clone(&self.agents);
        let security = target_policy;
        let global_credential = self.global_credential.clone();
        let provider_runtime_options = self.provider_runtime_options.clone();
        let depth = self.depth;
        let parent_tools = Arc::clone(&self.parent_tools);
        let multimodal_config = self.multimodal_config.clone();
        let delegate_config = self.delegate_config.clone();
        let workspace_dir = self.workspace_dir.clone();
        let child_token = self.cancellation_token.child_token();
        let task_id_clone = task_id.clone();
        let providers_models = Arc::clone(&self.providers_models);
        let risk_profiles = Arc::clone(&self.risk_profiles);
        let runtime_profiles = Arc::clone(&self.runtime_profiles);
        let skill_bundles = Arc::clone(&self.skill_bundles);
        let root_config = self.root_config.clone();
        let caller_alias = self.caller_alias.clone();
        let memory = self.memory.clone();
        // Capture the parent loop's session-key task-local so the
        // detached background task scopes its tool calls under the
        // same key — channel tools (sessions_send, etc.) need the
        // session key in scope to attribute correctly. Without this
        // wrap, the spawned task would lose the parent's task-local
        // and channel-scoped tool calls would land unattributed.
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
                    result = Box::pin(inner.execute_sync(&agent_name_owned, &full_prompt, &args_inner)) => {
                        match result {
                            Ok(tool_result) => {
                                if tool_result.success {
                                    Ok(tool_result.output)
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
            ),
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
                output: String::new(),
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
                output: String::new(),
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
                    output: String::new(),
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

        let mut target_policies: HashMap<String, Arc<SecurityPolicy>> =
            HashMap::with_capacity(agent_names.len());
        for name in &agent_names {
            match self.policy_for_target(name) {
                Ok(p) => {
                    target_policies.insert(name.clone(), p);
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("{e:#}")),
                    });
                }
            }
        }

        // Capture the current receipt scope so each spawned sub-agent task
        // re-enters it. Spawned tasks do not propagate task-locals, so
        // without this `execute_sync`'s `try_with` would resolve to `None`
        // inside the spawn and the parallel agents would run unsigned even
        // when the parent turn has receipts enabled. The collector is `Arc`'d
        // inside `ReceiptScope`, so all parallel agents push into the same
        // per-turn collector the orchestrator renders after the loop returns.
        let parent_receipt_scope = crate::agent::tool_receipts::TOOL_LOOP_RECEIPT_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .flatten();
        let parent_session_key = current_tool_loop_session_key();

        // Spawn all agents concurrently
        let mut handles = Vec::with_capacity(agent_names.len());
        for agent_name in &agent_names {
            let agents = Arc::clone(&self.agents);
            let security = target_policies
                .get(agent_name)
                .cloned()
                .unwrap_or_else(|| Arc::clone(&self.security));
            let global_credential = self.global_credential.clone();
            let provider_runtime_options = self.provider_runtime_options.clone();
            let depth = self.depth;
            let parent_tools = Arc::clone(&self.parent_tools);
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
            ),
            error: if all_success {
                None
            } else {
                Some("One or more parallel agents failed".into())
            },
        })
    }

    // ── Result Retrieval ────────────────────────────────────────────

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
                output: String::new(),
                error: Some(e),
            });
        }

        let result_path = self.results_dir().join(format!("{task_id}.json"));
        if !result_path.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("No result found for task_id '{task_id}'")),
            });
        }

        let content = tokio::fs::read_to_string(&result_path).await?;
        let result: BackgroundDelegateResult = serde_json::from_str(&content)?;

        Ok(ToolResult {
            success: result.status == BackgroundTaskStatus::Completed,
            output: serde_json::to_string_pretty(&result)?,
            error: if result.status == BackgroundTaskStatus::Completed {
                None
            } else {
                result.error
            },
        })
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
                results.push(json!({
                    "task_id": result.task_id,
                    "agent": result.agent,
                    "status": result.status,
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
            output: serde_json::to_string_pretty(&results)?,
            error: None,
        })
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
                output: String::new(),
                error: Some(e),
            });
        }

        let result_path = self.results_dir().join(format!("{task_id}.json"));
        if !result_path.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("No task found for task_id '{task_id}'")),
            });
        }

        // Read current status
        let content = tokio::fs::read_to_string(&result_path).await?;
        let mut result: BackgroundDelegateResult = serde_json::from_str(&content)?;

        if result.status != BackgroundTaskStatus::Running {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Task '{task_id}' is not running (status: {:?})",
                    result.status
                )),
            });
        }

        // Cancel via the parent token — this will cascade to all child tokens
        // Note: individual task cancellation uses the shared parent token, which
        // cancels all background tasks. For per-task cancellation, each background
        // task uses a child token, and the parent token cancels all.
        // We update the result file to reflect the cancellation request.
        result.status = BackgroundTaskStatus::Cancelled;
        result.error = Some("Cancelled by user request".into());
        result.finished_at = Some(chrono::Utc::now().to_rfc3339());
        Self::write_result_atomic(&result_path, &result).await?;

        Ok(ToolResult {
            success: true,
            output: format!("Task '{task_id}' cancellation requested."),
            error: None,
        })
    }

    /// Cancel all background tasks (cascade control).
    /// Call this when the parent session ends.
    pub fn cancel_all_background_tasks(&self) {
        self.cancellation_token.cancel();
    }

    /// Build an enriched system prompt for a sub-agent by composing structured
    /// operational sections (tools, skills, workspace, datetime, shell policy)
    /// with the per-agent identity files loaded from the target's own
    /// workspace dir (`<install>/agents/<alias>/workspace/AGENTS.md`,
    /// `SOUL.md`, `IDENTITY.md`, `USER.md`, `TOOLS.md`, `BOOTSTRAP.md`,
    /// `MEMORY.md`).
    fn build_enriched_system_prompt(
        &self,
        agent_alias: &str,
        agent_config: &AliasedAgentConfig,
        model_name: &str,
        sub_tools: &[Box<dyn Tool>],
        workspace_dir: &Path,
        sends_native_tool_specs: bool,
    ) -> Option<String> {
        // Resolve skill bundle directories. With one or more configured
        // bundles, load + concat skills from each. With none, fall back to
        // the workspace default.
        let bundle_dirs = self.resolve_skill_bundle_dirs(&agent_config.skill_bundles);
        let skills = if bundle_dirs.is_empty() {
            let default_dir = crate::skills::skills_dir(workspace_dir);
            crate::skills::load_skills_from_directory(&default_dir, false)
        } else {
            bundle_dirs
                .into_iter()
                .flat_map(|dir| {
                    crate::skills::load_skills_from_directory(&workspace_dir.join(dir), false)
                })
                .collect()
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
            skills: &skills,
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

        // Append the per-agent identity files from the target
        // sub-agent's own workspace dir. Each missing file is silently
        // skipped — the operator may not have authored every file.
        // Skipped entirely when no `root_config` is attached (legacy
        // unit-test constructors); production paths always attach it.
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
        let Some(tool_policy) = self.resolve_tool_policy(&agent_config.risk_profile) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' is agentic but risk_profile '{}' is not configured",
                    agent_config.risk_profile
                )),
            });
        };

        let target_policy = match self.policy_for_target(agent_name) {
            Ok(policy) => policy,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{e:#}")),
                });
            }
        };
        let needs_memory_tools = {
            let parent_tools = self.parent_tools.read();
            parent_tools.iter().any(|tool| {
                zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&tool.name())
                    && Self::delegate_admits_with_mcp(&tool_policy, tool.name())
            })
        };
        let mut target_memory_tools: HashMap<String, Box<dyn Tool>> = if needs_memory_tools {
            match self.memory_for_target_agent(agent_name).await {
                Ok(Some(memory)) => Self::memory_tools_for_target(memory, target_policy)
                    .into_iter()
                    .map(|tool| (tool.name().to_string(), tool))
                    .collect(),
                Ok(None) => HashMap::new(),
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Failed to initialize memory for delegate target '{agent_name}': {e:#}"
                        )),
                    });
                }
            }
        } else {
            HashMap::new()
        };

        let sub_tools: Vec<Box<dyn Tool>> = {
            let parent_tools = self.parent_tools.read();
            parent_tools
                .iter()
                .filter(|tool| tool.name() != Self::NAME)
                .filter(|tool| Self::delegate_admits_with_mcp(&tool_policy, tool.name()))
                .map(|tool| {
                    target_memory_tools
                        .remove(tool.name())
                        .unwrap_or_else(|| Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>)
                })
                .collect()
        };

        if sub_tools.is_empty() {
            let suffix = match (
                tool_policy.allowed_tools.as_ref(),
                tool_policy.excluded_tools.as_ref(),
            ) {
                (None, None) => "available from parent registry".to_string(),
                (Some(allowed), None) => {
                    format!("after filtering allowlist ({})", allowed.join(", "))
                }
                (None, Some(excluded)) => {
                    format!("after filtering denylist ({})", excluded.join(", "))
                }
                (Some(allowed), Some(excluded)) => format!(
                    "after filtering allowlist ({}) and denylist ({})",
                    allowed.join(", "),
                    excluded.join(", ")
                ),
            };
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' has no executable tools {suffix}"
                )),
            });
        }

        let loop_runtime = self.resolve_loop_runtime(agent_name, agent_config);
        let mut prompt_agent_config = agent_config.clone();
        prompt_agent_config.resolved = loop_runtime.clone();

        // Build enriched system prompt with tools, skills, workspace, datetime context.
        let enriched_system_prompt = self.build_enriched_system_prompt(
            agent_name,
            &prompt_agent_config,
            model,
            &sub_tools,
            &self.workspace_dir,
            model_provider.supports_native_tools(),
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
        // Forward the per-turn receipt scope from the parent loop so subagent
        // tool calls land in the same collector as the top-level turn. When
        // receipts are disabled (or no scope is set, e.g. CLI / background
        // delegate spawn) this resolves to `None` and the sub-loop runs
        // unsigned, matching the parent.
        let receipt_scope = crate::agent::tool_receipts::TOOL_LOOP_RECEIPT_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .flatten();
        let receipt_generator = receipt_scope.as_ref().map(|s| &s.generator);
        let collected_receipts = receipt_scope.as_ref().map(|s| s.collector.as_ref());
        let result = tokio::time::timeout(
            Duration::from_secs(agentic_timeout_secs),
            run_tool_call_loop(
                model_provider,
                &mut history,
                &sub_tools,
                &noop_observer,
                provider_type,
                model,
                temperature,
                true,
                None,
                "delegate",
                None,
                &self.multimodal_config,
                loop_runtime.max_tool_iterations,
                Some(self.cancellation_token.child_token()),
                None,
                None,
                &[],
                tool_policy.excluded_tools.as_deref().unwrap_or(&[]),
                None,
                None,
                &zeroclaw_config::schema::PacingConfig::default(),
                loop_runtime.strict_tool_parsing,
                loop_runtime.parallel_tools,
                loop_runtime.max_tool_result_chars,
                // Keep delegate subagent context pruning aligned with top-level
                // agents instead of preserving the old disabled-by-zero path.
                loop_runtime.max_context_tokens,
                None, // shared_budget: TODO thread from parent in future
                None, // channel: delegate subagents don't support approval
                receipt_generator,
                collected_receipts,
                None, // event_tx
                None, // steering
                None, // new_messages_out
                &LoopKnobs::default(),
                None,
            )
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
                    ),
                    error: None,
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Agent '{agent_name}' failed: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
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
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::{MemoryRecallTool, MemoryStoreTool};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use tokio::time::{Instant, sleep};
    use zeroclaw_config::schema::{
        Config, CustomModelProviderConfig, DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS,
        DEFAULT_DELEGATE_TIMEOUT_SECS, ModelProviderConfig,
    };
    use zeroclaw_memory::{AgentScopedMemory, SqliteMemory};
    use zeroclaw_providers::{ChatRequest, ChatResponse, ToolCall};

    zeroclaw_api::mock_tool_attribution!(EchoTool, FakeMcpTool);

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
                output: format!("echo:{value}"),
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

    fn agentic_providers_models() -> HashMap<String, HashMap<String, ModelProviderConfig>> {
        let mut models: HashMap<String, HashMap<String, ModelProviderConfig>> = HashMap::new();
        models.entry("openrouter".to_string()).or_default().insert(
            "agentic".to_string(),
            ModelProviderConfig {
                model: Some("model-test".to_string()),
                temperature: Some(0.2),
                api_key: Some("delegate-test-credential".to_string()),
                ..Default::default()
            },
        );
        models
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

    async fn assert_stored_for_target_only(fixture: &DelegateMemoryFixture, key: &str) {
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
        let mut config = Config::default();
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
        config.agents.get_mut("aaa").unwrap().delegates = vec!["aaalore".to_string()];
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
        aaa.delegates = vec!["aaalore".to_string()];
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
        // caller's already-filtered tools instead of being rejected (#7470).
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(Vec::new()))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(EchoTool),
                Arc::new(DelegateTool::new(HashMap::new(), None, test_security())),
            ])));

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
        assert!(result.output.contains("(openrouter/model-test, agentic)"));
        assert!(result.output.contains("done"));
    }

    #[tokio::test]
    async fn agentic_mode_empty_allowed_tools_empty_registry_errors_gracefully() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_agent_config());

        let tool = DelegateTool::new(agents, None, test_security())
            .with_providers_models(agentic_providers_models())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(Vec::new()));
        let result = tool
            .execute(json!({"agent": "agentic", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no executable tools available from parent registry"),
            "got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn agentic_mode_empty_allowed_tools_respects_excluded_tools() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_agent_config());

        let tool = DelegateTool::new(agents, None, test_security())
            .with_providers_models(agentic_providers_models())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles_with_excluded(
                Vec::new(),
                vec!["echo_tool".to_string()],
            ))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let result = tool
            .execute(json!({"agent": "agentic", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no executable tools after filtering denylist (echo_tool)")
        );
    }

    #[tokio::test]
    async fn agentic_mode_padded_allowed_tool_name_remains_exact() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_agent_config());

        let tool = DelegateTool::new(agents, None, test_security())
            .with_providers_models(agentic_providers_models())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(vec![" echo_tool ".to_string()]))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let result = tool
            .execute(json!({"agent": "agentic", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no executable tools after filtering allowlist")
        );
    }

    #[tokio::test]
    async fn agentic_mode_rejects_unmatched_allowed_tools() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_agent_config());

        let allowed = vec!["missing_tool".to_string()];
        let tool = DelegateTool::new(agents, None, test_security())
            .with_providers_models(agentic_providers_models())
            .with_runtime_profiles(agentic_runtime_profiles(10))
            .with_risk_profiles(agentic_risk_profiles(allowed))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));
        let result = tool
            .execute(json!({"agent": "agentic", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no executable tools")
        );
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

        assert!(result.success);
        assert!(result.output.contains("(openrouter/model-test, agentic)"));
        assert!(result.output.contains("done"));
    }

    #[tokio::test]
    async fn execute_agentic_rebinds_memory_tools_to_target_agent_scope() {
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
    async fn execute_agentic_strict_tool_parsing_uses_target_agent_policy() {
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

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no executable tools")
        );
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
        // Receipt forwarding through the delegate sub-loop is the activation
        // pass for #6182's delegate.rs:1184 acceptance criterion. With
        // `TOOL_LOOP_RECEIPT_CONTEXT` scoped, every sub-tool call inside the
        // delegate must produce a receipt that lands in the same per-turn
        // collector the parent passed in. Without the task-local read in
        // `execute_sync` this test fails: the collector stays empty because
        // the sub-loop runs unsigned with `None, None` for the receipt args.
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

    /// PR #7547 review (Audacity88): the `<server>__<tool>` MCP-naming
    /// convention must be auto-admitted by the delegate filter even when
    /// the risk profile's explicit `allowed_tools` list does not mention
    /// the tool. The pre-existing `mcp_tools_included_in_subagent_tool_list`
    /// fixture uses `mcp_fake` (no double underscore) so it exercises the
    /// explicit allow-list path, not the new auto-admit branch. This test
    /// pins the new branch via the resolve_tool_policy + delegate_admits_with_mcp
    /// pair that replaces the pre-#7608 resolve_allowed_tools helper.
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

    /// PR #7547 review (Audacity88) — blocking comment: the PR body
    /// claims MCP tools can still be blocked via `excluded_tools`. A
    /// target profile that allow-lists `shell` and excludes
    /// `filesystem__write_file` must NOT receive the MCP wrapper in an
    /// agentic delegate even though it matches the `__` auto-admit
    /// heuristic.
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

    /// Companion to the test above: `excluded_tools` must also subtract
    /// from explicit allow-list entries, not just from the
    /// double-underscore auto-admit set. delegate.rs previously ignored
    /// `excluded_tools` entirely on the agentic path; this pins the fix
    /// so it cannot regress.
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
            .build_enriched_system_prompt("alpha", &config, "test-model", &tools, &workspace, false)
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
        // Identity files come from the target sub-agent's per-agent
        // workspace dir. The test's install_root is unset, so no
        // identity files exist for the dummy alias — the prompt still
        // contains the structural sections verified above, which is
        // the load-bearing assertion.

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
                    output: String::new(),
                    error: None,
                })
            }
        }

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockShellTool)];
        let workspace = std::env::temp_dir();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.to_path_buf());

        let prompt = tool
            .build_enriched_system_prompt("alpha", &config, "test-model", &tools, &workspace, false)
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
            .build_enriched_system_prompt("alpha", &config, "test-model", &tools, &workspace, false)
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
            .build_enriched_system_prompt("alpha", &config, "test-model", &tools, &workspace, false)
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
            .build_enriched_system_prompt("alpha", &config, "test-model", &tools, &workspace, false)
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
        let mut config = Config::default();
        // The caller delegates from the `narrow` profile, so that profile must
        // allow delegation before the same-profile gate under test is reached.
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

    #[tokio::test]
    async fn delegate_rejects_cross_profile_target_not_in_roster() {
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
    }

    #[tokio::test]
    async fn delegate_rejects_explicit_cross_profile_target_that_escalates() {
        // target sits on the wider profile (higher max_actions). Listing it
        // explicitly clears the reachability gate, but the escalation guard
        // must still refuse because the target would widen the caller.
        let config = config_with_two_agents("caller", 5, "target", 50);
        let mut config = (*config).clone();
        config
            .agents
            .get_mut("caller")
            .unwrap()
            .delegates
            .push("target".to_string());
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

        let err = tool
            .policy_for_target("target")
            .expect_err("escalating cross-profile delegate must be rejected");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("escalate"),
            "expected escalation rejection, got: {chain}"
        );
    }

    #[tokio::test]
    async fn delegate_allows_explicit_cross_profile_target_that_narrows() {
        // target on a narrower profile (lower max_actions) is a valid
        // explicit delegate: in the roster and non-escalating.
        let config = config_with_two_agents("caller", 50, "target", 5);
        let mut config = (*config).clone();
        config
            .agents
            .get_mut("caller")
            .unwrap()
            .delegates
            .push("target".to_string());
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
            .expect("non-escalating target resolves");
        assert!(
            !target_policy.tracker.record_within(bucket_key, max),
            "delegated target must consume from the caller's bucket; spawning the target should not reset the budget"
        );
    }

    /// Regression for issue #7263: when the caller's policy was built
    /// with a session cwd (the ACP / gateway path), delegating to a
    /// sibling agent must carry that cwd into the target's policy.
    /// Without this, the child run's file/shell tools jail to the
    /// per-agent install dir declared in config rather than the IDE's
    /// session cwd, breaking delegate-driven workflows in repos
    /// outside the install root.
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

    /// Build a config where `caller` (`broad` profile) is authorized to
    /// delegate to `target`, but `target` sits on a different (`narrow`)
    /// profile. Delegation requires caller and target to share a risk
    /// profile, so this exercises the same-profile rejection gate.
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

        // Regression guard: the pre-fix path (bare factory, no config/alias
        // context) cannot see the per-alias config — for the custom family it
        // errors on the missing uri it can't resolve, which is exactly the
        // "error in the provider" the bug report described. Either way it does
        // not yield a working responses provider.
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
}
