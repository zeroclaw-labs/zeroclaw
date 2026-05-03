use crate::agent::loop_::run_tool_call_loop;
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
    DelegateAgentConfig, DelegateToolConfig, MemoryNamespaceConfig, ModelProviderConfig,
    RiskProfileConfig, RuntimeProfileConfig, SkillBundleConfig,
};
use zeroclaw_memory::{Memory, NamespacedMemory};
use zeroclaw_providers::{self, ChatMessage, Provider};

/// Default temperature for sub-agent tool loops when the delegate config
/// leaves it unset; matches the longstanding agentic default that balances
/// coherence with enough variety to explore tool options.
const DELEGATE_AGENTIC_DEFAULT_TEMPERATURE: f64 = 0.7;

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
/// provider/model configuration. Enables multi-agent workflows where
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
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    security: Arc<SecurityPolicy>,
    /// Global credential (from config.api_key) used when an agent has none set.
    global_credential: Option<String>,
    /// Provider runtime options inherited from root config.
    provider_runtime_options: zeroclaw_providers::ProviderRuntimeOptions,
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
    /// V3: nested model-provider map for brain resolution.
    providers_models: Arc<HashMap<String, HashMap<String, ModelProviderConfig>>>,
    /// V3: named risk profiles for delegation depth and timeout resolution.
    risk_profiles: Arc<HashMap<String, RiskProfileConfig>>,
    /// V3: named runtime profiles for agentic/tools/iteration resolution.
    runtime_profiles: Arc<HashMap<String, RuntimeProfileConfig>>,
    /// V3: named skill bundles for skills-directory resolution.
    skill_bundles: Arc<HashMap<String, SkillBundleConfig>>,
    /// V3: named memory namespaces for isolation resolution.
    memory_namespaces: Arc<HashMap<String, MemoryNamespaceConfig>>,
}

impl DelegateTool {
    pub fn new(
        agents: HashMap<String, DelegateAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self::new_with_options(
            agents,
            global_credential,
            security,
            zeroclaw_providers::ProviderRuntimeOptions::default(),
        )
    }

    pub fn new_with_options(
        agents: HashMap<String, DelegateAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: zeroclaw_providers::ProviderRuntimeOptions,
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
            memory_namespaces: Arc::new(HashMap::new()),
        }
    }

    /// Create a DelegateTool for a sub-agent (with incremented depth).
    /// When sub-agents eventually get their own tool registry, construct
    /// their DelegateTool via this method with `depth: parent.depth + 1`.
    pub fn with_depth(
        agents: HashMap<String, DelegateAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
    ) -> Self {
        Self::with_depth_and_options(
            agents,
            global_credential,
            security,
            depth,
            zeroclaw_providers::ProviderRuntimeOptions::default(),
        )
    }

    pub fn with_depth_and_options(
        agents: HashMap<String, DelegateAgentConfig>,
        global_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
        provider_runtime_options: zeroclaw_providers::ProviderRuntimeOptions,
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
            memory_namespaces: Arc::new(HashMap::new()),
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

    /// Attach V3 nested model-provider map for brain resolution.
    pub fn with_providers_models(
        mut self,
        m: HashMap<String, HashMap<String, ModelProviderConfig>>,
    ) -> Self {
        self.providers_models = Arc::new(m);
        self
    }

    /// Attach V3 risk profiles for depth/timeout resolution.
    pub fn with_risk_profiles(mut self, m: HashMap<String, RiskProfileConfig>) -> Self {
        self.risk_profiles = Arc::new(m);
        self
    }

    /// Attach V3 runtime profiles for agentic/tools/iteration resolution.
    pub fn with_runtime_profiles(mut self, m: HashMap<String, RuntimeProfileConfig>) -> Self {
        self.runtime_profiles = Arc::new(m);
        self
    }

    /// Attach V3 skill bundles for skills-directory resolution.
    pub fn with_skill_bundles(mut self, m: HashMap<String, SkillBundleConfig>) -> Self {
        self.skill_bundles = Arc::new(m);
        self
    }

    /// Attach V3 memory namespaces for isolation resolution.
    pub fn with_memory_namespaces(mut self, m: HashMap<String, MemoryNamespaceConfig>) -> Self {
        self.memory_namespaces = Arc::new(m);
        self
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

    /// Resolve max delegation depth from the named risk profile (default: 3).
    fn resolve_max_depth(&self, risk_profile: &str) -> u32 {
        if risk_profile.is_empty() {
            return 3;
        }
        self.risk_profiles
            .get(risk_profile)
            .map(|p| p.max_delegation_depth)
            .filter(|&d| d > 0)
            .unwrap_or(3)
    }

    /// Resolve per-call delegation timeout from the named risk profile.
    fn resolve_delegation_timeout(&self, risk_profile: &str) -> Option<u64> {
        if risk_profile.is_empty() {
            return None;
        }
        self.risk_profiles
            .get(risk_profile)
            .and_then(|p| p.delegation_timeout_secs)
    }

    /// Resolve agentic run timeout from the named risk profile.
    fn resolve_agentic_timeout_secs(&self, risk_profile: &str) -> Option<u64> {
        if risk_profile.is_empty() {
            return None;
        }
        self.risk_profiles
            .get(risk_profile)
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

    /// Resolve max tool iterations from the named runtime profile (default: 10).
    fn resolve_max_iterations(&self, runtime_profile: &str) -> usize {
        if runtime_profile.is_empty() {
            return 10;
        }
        self.runtime_profiles
            .get(runtime_profile)
            .map(|p| p.max_tool_iterations)
            .filter(|&i| i > 0)
            .unwrap_or(10)
    }

    /// Resolve allowed tools list from the named runtime profile.
    fn resolve_allowed_tools(&self, runtime_profile: &str) -> Vec<String> {
        if runtime_profile.is_empty() {
            return Vec::new();
        }
        self.runtime_profiles
            .get(runtime_profile)
            .map(|p| p.allowed_tools.clone())
            .unwrap_or_default()
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

    /// Resolve memory namespace string from the named memory namespace config.
    fn resolve_memory_ns(&self, memory_namespace: &str) -> Option<String> {
        if memory_namespace.is_empty() {
            return None;
        }
        self.memory_namespaces
            .get(memory_namespace)
            .map(|n| n.namespace.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| Some(memory_namespace.to_string()))
    }

    /// Wrap memory with namespace isolation if configured for the given agent.
    /// Returns the namespaced memory if memory_namespace is set, otherwise returns
    /// the original memory.
    #[allow(dead_code)] // WIP: will be used when delegate agents support memory
    fn get_agent_memory(&self, agent_config: &DelegateAgentConfig) -> Option<Arc<dyn Memory>> {
        self.memory.as_ref().map(|mem| {
            if let Some(ns) = self.resolve_memory_ns(&agent_config.memory_namespace) {
                Arc::new(NamespacedMemory::new(mem.clone(), ns)) as Arc<dyn Memory>
            } else {
                mem.clone()
            }
        })
    }

    /// Directory where background delegate results are stored.
    fn results_dir(&self) -> PathBuf {
        self.workspace_dir.join("delegate_results")
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
        "delegate"
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
        let agent_names: Vec<&str> = self.agents.keys().map(|s: &String| s.as_str()).collect();
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
            .ok_or_else(|| anyhow::anyhow!("Missing 'agent' parameter"))?;

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
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter"))?;

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

        // Resolve V3 profile references
        let max_depth = self.resolve_max_depth(&agent_config.risk_profile);
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

        // Create provider for this agent
        let provider: Box<dyn Provider> = match zeroclaw_providers::create_provider_with_options(
            &provider_type,
            credential.as_deref(),
            &self.provider_runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Failed to create provider '{provider_type}' for agent '{agent_name}': {e}"
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
                    &*provider,
                    &full_prompt,
                    temperature.unwrap_or(DELEGATE_AGENTIC_DEFAULT_TEMPERATURE),
                )
                .await;
        }

        // Build enriched system prompt for non-agentic sub-agent.
        let enriched_system_prompt =
            self.build_enriched_system_prompt(agent_config, &model, &[], &self.workspace_dir);
        let system_prompt_ref = enriched_system_prompt.as_deref();

        // Wrap the provider call in a timeout to prevent indefinite blocking
        let timeout_secs = self
            .resolve_delegation_timeout(&agent_config.risk_profile)
            .unwrap_or(self.delegate_config.timeout_secs);
        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            provider.chat_with_system(system_prompt_ref, &full_prompt, &model, temperature),
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

        let max_depth = self.resolve_max_depth(&agent_config.risk_profile);
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
        let json_bytes = serde_json::to_vec_pretty(&initial_result)?;
        tokio::fs::write(&result_path, &json_bytes).await?;

        // Clone everything needed for the spawned task
        let agents = Arc::clone(&self.agents);
        let security = Arc::clone(&self.security);
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
        let memory_namespaces = Arc::clone(&self.memory_namespaces);

        tokio::spawn(async move {
            // Build an inner DelegateTool for the spawned context
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
                memory: None,
                providers_models,
                risk_profiles,
                runtime_profiles,
                skill_bundles,
                memory_namespaces,
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
            if let Ok(bytes) = serde_json::to_vec_pretty(&final_result) {
                let _ = tokio::fs::write(&result_path, &bytes).await;
            }
        });

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
            .ok_or_else(|| anyhow::anyhow!("Missing 'prompt' parameter for parallel execution"))?;

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

        // Spawn all agents concurrently
        let mut handles = Vec::with_capacity(agent_names.len());
        for agent_name in &agent_names {
            let agents = Arc::clone(&self.agents);
            let security = Arc::clone(&self.security);
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
            let memory_namespaces = Arc::clone(&self.memory_namespaces);

            handles.push(tokio::spawn(async move {
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
                    memory: None,
                    providers_models,
                    risk_profiles,
                    runtime_profiles,
                    skill_bundles,
                    memory_namespaces,
                };
                let result = Box::pin(inner.execute_sync(&agent_name, &prompt, &args_clone)).await;
                (agent_name, result)
            }));
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
            .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' parameter for check_result"))?;

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
            .ok_or_else(|| anyhow::anyhow!("Missing 'task_id' parameter for cancel_task"))?;

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
        let bytes = serde_json::to_vec_pretty(&result)?;
        tokio::fs::write(&result_path, &bytes).await?;

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
    /// with the operator-configured `system_prompt` string.
    fn build_enriched_system_prompt(
        &self,
        agent_config: &DelegateAgentConfig,
        model_name: &str,
        sub_tools: &[Box<dyn Tool>],
        workspace_dir: &Path,
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
        let has_shell = sub_tools.iter().any(|t| t.name() == "shell");
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
            model_name,
            tools: sub_tools,
            skills: &skills,
            skills_prompt_mode: zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",

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

        // Append the operator-configured system_prompt as the identity/role block.
        if let Some(operator_prompt) = agent_config.system_prompt.as_ref() {
            enriched.push_str(operator_prompt);
            enriched.push('\n');
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
        agent_config: &DelegateAgentConfig,
        provider_type: &str,
        model: &str,
        provider: &dyn Provider,
        full_prompt: &str,
        temperature: f64,
    ) -> anyhow::Result<ToolResult> {
        let allowed_tools = self.resolve_allowed_tools(&agent_config.runtime_profile);

        if allowed_tools.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' is agentic but runtime_profile '{}' has no allowed_tools",
                    agent_config.runtime_profile
                )),
            });
        }

        let allowed = allowed_tools
            .iter()
            .map(|name: &String| name.trim())
            .filter(|name| !name.is_empty())
            .collect::<std::collections::HashSet<_>>();

        let sub_tools: Vec<Box<dyn Tool>> = {
            let parent_tools = self.parent_tools.read();
            parent_tools
                .iter()
                .filter(|tool| allowed.contains(tool.name()))
                .filter(|tool| tool.name() != "delegate")
                .map(|tool| Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>)
                .collect()
        };

        if sub_tools.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' has no executable tools after filtering allowlist ({})",
                    allowed_tools.join(", ")
                )),
            });
        }

        let max_iterations = self.resolve_max_iterations(&agent_config.runtime_profile);

        // Build enriched system prompt with tools, skills, workspace, datetime context.
        let enriched_system_prompt =
            self.build_enriched_system_prompt(agent_config, model, &sub_tools, &self.workspace_dir);

        let mut history = Vec::new();
        if let Some(system_prompt) = enriched_system_prompt.as_ref() {
            history.push(ChatMessage::system(system_prompt.clone()));
        }
        history.push(ChatMessage::user(full_prompt.to_string()));

        let noop_observer = NoopObserver;

        let agentic_timeout_secs = self
            .resolve_agentic_timeout_secs(&agent_config.risk_profile)
            .unwrap_or(self.delegate_config.agentic_timeout_secs);
        let result = tokio::time::timeout(
            Duration::from_secs(agentic_timeout_secs),
            run_tool_call_loop(
                provider,
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
                max_iterations,
                Some(self.cancellation_token.child_token()),
                None,
                None,
                &[],
                &[],
                None,
                None,
                &zeroclaw_config::schema::PacingConfig::default(),
                0,    // max_tool_result_chars: inherit from parent config in future
                0,    // context_token_budget: 0 = disabled for subagents
                None, // shared_budget: TODO thread from parent in future
                None, // channel: delegate subagents don't support approval
                None, // receipt_generator
                None, // collected_receipts
            ),
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
    use anyhow::anyhow;
    use zeroclaw_config::schema::{
        DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS, DEFAULT_DELEGATE_TIMEOUT_SECS,
    };
    use zeroclaw_providers::{ChatRequest, ChatResponse, ToolCall};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn sample_agents() -> HashMap<String, DelegateAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                system_prompt: Some("You are a research assistant.".to_string()),
                model_provider: "ollama.researcher".to_string(),
                ..Default::default()
            },
        );
        agents.insert(
            "coder".to_string(),
            DelegateAgentConfig {
                model_provider: "openrouter.coder".to_string(),
                ..Default::default()
            },
        );
        agents
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

    struct OneToolThenFinalProvider;

    #[async_trait]
    impl Provider for OneToolThenFinalProvider {
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

    struct InfiniteToolCallProvider;

    #[async_trait]
    impl Provider for InfiniteToolCallProvider {
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

    struct FailingProvider;

    #[async_trait]
    impl Provider for FailingProvider {
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
            Err(anyhow!("provider boom"))
        }
    }

    fn agentic_agent_config() -> DelegateAgentConfig {
        DelegateAgentConfig {
            system_prompt: Some("You are agentic.".to_string()),
            model_provider: "openrouter.agentic".to_string(),
            runtime_profile: "agentic_test".to_string(),
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

    fn agentic_runtime_profiles(
        allowed_tools: Vec<String>,
        max_iterations: usize,
    ) -> HashMap<String, RuntimeProfileConfig> {
        let mut profiles = HashMap::new();
        profiles.insert(
            "agentic_test".to_string(),
            RuntimeProfileConfig {
                agentic: true,
                allowed_tools,
                max_tool_iterations: max_iterations,
                ..Default::default()
            },
        );
        profiles
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
        let tool = DelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher") || desc.contains("coder"));
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
            DelegateAgentConfig {
                model_provider: "totally-invalid-provider.default".to_string(),
                ..Default::default()
            },
        );
        let tool = DelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({"agent": "broken", "prompt": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Failed to create provider"));
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
        // Should find "researcher" after trim — will fail at provider level
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
            DelegateAgentConfig {
                model_provider: "invalid-for-test.default".to_string(),
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
                .contains("Failed to create provider")
        );
    }

    #[tokio::test]
    async fn delegate_empty_context_omits_prefix() {
        let mut agents = HashMap::new();
        agents.insert(
            "tester".to_string(),
            DelegateAgentConfig {
                model_provider: "invalid-for-test.default".to_string(),
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
                .contains("Failed to create provider")
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
    async fn agentic_mode_rejects_empty_allowed_tools() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_agent_config());

        let tool = DelegateTool::new(agents, None, test_security())
            .with_providers_models(agentic_providers_models())
            .with_runtime_profiles(agentic_runtime_profiles(Vec::new(), 10));
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
                .contains("has no allowed_tools"),
            "got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn agentic_mode_rejects_unmatched_allowed_tools() {
        let mut agents = HashMap::new();
        agents.insert("agentic".to_string(), agentic_agent_config());

        let tool = DelegateTool::new(agents, None, test_security())
            .with_providers_models(agentic_providers_models())
            .with_runtime_profiles(agentic_runtime_profiles(
                vec!["missing_tool".to_string()],
                10,
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
                .contains("no executable tools")
        );
    }

    #[tokio::test]
    async fn execute_agentic_runs_tool_call_loop_with_filtered_tools() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(vec!["echo_tool".to_string()], 10))
            .with_parent_tools(Arc::new(RwLock::new(vec![
                Arc::new(EchoTool),
                Arc::new(DelegateTool::new(HashMap::new(), None, test_security())),
            ])));

        let provider = OneToolThenFinalProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &provider,
                "run",
                0.2,
            )
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("(openrouter/model-test, agentic)"));
        assert!(result.output.contains("done"));
    }

    #[tokio::test]
    async fn execute_agentic_excludes_delegate_even_if_allowlisted() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(vec!["delegate".to_string()], 10))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(DelegateTool::new(
                HashMap::new(),
                None,
                test_security(),
            ))])));

        let provider = OneToolThenFinalProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &provider,
                "run",
                0.2,
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
            .with_runtime_profiles(agentic_runtime_profiles(vec!["echo_tool".to_string()], 2))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let provider = InfiniteToolCallProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &provider,
                "run",
                0.2,
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
    async fn execute_agentic_propagates_provider_errors() {
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(vec!["echo_tool".to_string()], 10))
            .with_parent_tools(Arc::new(RwLock::new(vec![Arc::new(EchoTool)])));

        let provider = FailingProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &provider,
                "run",
                0.2,
            )
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("provider boom")
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

    struct McpToolThenFinalProvider;

    #[async_trait]
    impl Provider for McpToolThenFinalProvider {
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

    #[tokio::test]
    async fn mcp_tools_included_in_subagent_tool_list() {
        // Build DelegateTool with NO parent tools initially
        let config = agentic_agent_config();
        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_runtime_profiles(agentic_runtime_profiles(vec!["mcp_fake".to_string()], 10))
            .with_parent_tools(Arc::new(RwLock::new(Vec::new())));

        // Simulate late MCP tool injection via the shared handle
        let handle = tool.parent_tools_handle();
        handle.write().push(Arc::new(FakeMcpTool));

        let provider = McpToolThenFinalProvider;
        let result = tool
            .execute_agentic(
                "agentic",
                &config,
                "openrouter",
                "model-test",
                &provider,
                "run mcp",
                0.2,
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
    fn enriched_prompt_includes_tools_workspace_datetime() {
        let config = DelegateAgentConfig {
            system_prompt: Some("You are a code reviewer.".to_string()),
            model_provider: "openrouter.test".to_string(),
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
            .build_enriched_system_prompt(&config, "test-model", &tools, &workspace)
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
            prompt.contains("## CRITICAL CONTEXT: CURRENT DATE & TIME"),
            "should contain datetime section"
        );
        assert!(
            prompt.contains("You are a code reviewer."),
            "should append operator system_prompt"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn enriched_prompt_includes_shell_policy_when_shell_present() {
        let config = DelegateAgentConfig::default();

        struct MockShellTool;
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
            .build_enriched_system_prompt(&config, "test-model", &tools, &workspace)
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
        let config = DelegateAgentConfig::default();

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];
        let workspace = std::env::temp_dir();

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.to_path_buf());

        let prompt = tool
            .build_enriched_system_prompt(&config, "test-model", &tools, &workspace)
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
        config
            .providers
            .models
            .entry("ollama".into())
            .or_default()
            .insert(
                "default".into(),
                zeroclaw_config::schema::ModelProviderConfig::default(),
            );
        config.agents.insert(
            "ok".into(),
            DelegateAgentConfig {
                model_provider: "ollama.default".into(),
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

        let config = DelegateAgentConfig {
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
            .build_enriched_system_prompt(&config, "test-model", &tools, &workspace)
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

        let config = DelegateAgentConfig::default();

        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoTool)];

        let tool = DelegateTool::new(HashMap::new(), None, test_security())
            .with_workspace_dir(workspace.clone());

        let prompt = tool
            .build_enriched_system_prompt(&config, "test-model", &tools, &workspace)
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

        // The agent will fail at provider level (ollama not running),
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

        // Wait for the background task to finish
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Check that the result file exists
        let result_path = workspace
            .join("delegate_results")
            .join(format!("{task_id}.json"));
        assert!(
            result_path.exists(),
            "Result file should exist at {result_path:?}"
        );

        // Read and parse the result
        let content = std::fs::read_to_string(&result_path).unwrap();
        let bg_result: BackgroundDelegateResult = serde_json::from_str(&content).unwrap();
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
        tokio::time::sleep(Duration::from_millis(500)).await;

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

        // Wait for task to complete
        tokio::time::sleep(Duration::from_millis(500)).await;

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
        // Should proceed to delegation (will fail at provider since ollama isn't running)
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
}
