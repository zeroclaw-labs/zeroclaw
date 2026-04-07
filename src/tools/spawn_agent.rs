use super::traits::{Tool, ToolResult};
use crate::agent::loop_::run_tool_call_loop;
use crate::agent::prompt::{PromptContext, SystemPromptBuilder};
use crate::config::{DelegateAgentConfig, DelegateToolConfig, SkillsPromptInjectionMode};
use crate::memory::{Memory, NamespacedMemory};
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::providers::{self, ChatMessage, Provider};
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use super::delegate::{BackgroundDelegateResult, BackgroundTaskStatus};

/// Tool that creates and runs ad-hoc sub-agents with inline specifications.
///
/// Unlike [`DelegateTool`](super::DelegateTool) which requires pre-configured
/// agents in `[agents.<name>]` config, `SpawnAgentTool` lets the LLM create
/// ephemeral agents on the fly with custom personas, tool sets, and execution
/// modes. Agents can optionally be persisted to the workspace for reuse.
pub struct SpawnAgentTool {
    /// Shared mutable agent registry (also used by DelegateTool and WorkspaceAgentManager).
    agents: Arc<RwLock<HashMap<String, DelegateAgentConfig>>>,
    /// Security policy.
    security: Arc<SecurityPolicy>,
    /// Global credential fallback (from config.api_key).
    fallback_credential: Option<String>,
    /// Provider runtime options inherited from root config.
    provider_runtime_options: providers::ProviderRuntimeOptions,
    /// Parent tool registry for filtering sub-agent tools.
    parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    /// Inherited multimodal handling config for sub-agent loops.
    multimodal_config: crate::config::MultimodalConfig,
    /// Global delegate tool config providing default timeout values.
    delegate_config: DelegateToolConfig,
    /// Workspace directory inherited from the root agent context.
    workspace_dir: PathBuf,
    /// Skills prompt injection mode inherited from root config.
    skills_prompt_mode: SkillsPromptInjectionMode,
    /// Optional memory instance for namespace isolation on spawned agents.
    memory: Option<Arc<dyn Memory>>,
    /// Default provider name for agents that don't specify one.
    default_provider: String,
    /// Default model name.
    default_model: String,
    /// Optional concurrency semaphore shared with DelegateTool.
    subagent_semaphore: Option<Arc<tokio::sync::Semaphore>>,
}

impl SpawnAgentTool {
    pub fn new(
        agents: Arc<RwLock<HashMap<String, DelegateAgentConfig>>>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: providers::ProviderRuntimeOptions,
    ) -> Self {
        Self {
            agents,
            security,
            fallback_credential,
            provider_runtime_options,
            parent_tools: Arc::new(RwLock::new(Vec::new())),
            multimodal_config: crate::config::MultimodalConfig::default(),
            delegate_config: DelegateToolConfig::default(),
            workspace_dir: PathBuf::new(),
            skills_prompt_mode: SkillsPromptInjectionMode::Full,
            memory: None,
            default_provider: "anthropic".into(),
            default_model: "claude-sonnet-4-20250514".into(),
            subagent_semaphore: None,
        }
    }

    pub fn with_parent_tools(mut self, parent_tools: Arc<RwLock<Vec<Arc<dyn Tool>>>>) -> Self {
        self.parent_tools = parent_tools;
        self
    }

    pub fn with_multimodal_config(mut self, config: crate::config::MultimodalConfig) -> Self {
        self.multimodal_config = config;
        self
    }

    pub fn with_delegate_config(mut self, config: DelegateToolConfig) -> Self {
        self.delegate_config = config;
        self
    }

    pub fn with_workspace_dir(mut self, workspace_dir: PathBuf) -> Self {
        self.workspace_dir = workspace_dir;
        self
    }

    pub fn with_skills_prompt_mode(mut self, mode: SkillsPromptInjectionMode) -> Self {
        self.skills_prompt_mode = mode;
        self
    }

    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_subagent_semaphore(mut self, sem: Arc<tokio::sync::Semaphore>) -> Self {
        self.subagent_semaphore = Some(sem);
        self
    }

    pub fn with_defaults(mut self, provider: String, model: String) -> Self {
        self.default_provider = provider;
        self.default_model = model;
        self
    }

    /// Wrap memory with namespace isolation if configured for the given agent.
    fn get_agent_memory(&self, agent_config: &DelegateAgentConfig) -> Option<Arc<dyn Memory>> {
        self.memory.as_ref().map(|mem| {
            if let Some(namespace) = &agent_config.memory_namespace {
                Arc::new(NamespacedMemory::new(mem.clone(), namespace.clone())) as Arc<dyn Memory>
            } else {
                mem.clone()
            }
        })
    }

    /// Directory where background delegate results are stored.
    fn results_dir(&self) -> PathBuf {
        self.workspace_dir.join("delegate_results")
    }

    /// Build an enriched system prompt for a spawned sub-agent by composing structured
    /// operational sections (tools, skills, workspace, datetime, shell policy)
    /// with the operator-configured `system_prompt` string.
    fn build_enriched_system_prompt(
        &self,
        agent_config: &DelegateAgentConfig,
        sub_tools: &[Box<dyn Tool>],
        workspace_dir: &Path,
    ) -> Option<String> {
        let skills_dir = agent_config
            .skills_directory
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .map(|dir| workspace_dir.join(dir))
            .unwrap_or_else(|| crate::skills::skills_dir(workspace_dir));
        let skills = crate::skills::load_skills_from_directory(&skills_dir, false);

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

        let ctx = PromptContext {
            workspace_dir,
            model_name: &agent_config.model,
            tools: sub_tools,
            skills: &skills,
            skills_prompt_mode: self.skills_prompt_mode,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
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

    /// Execute the spawned agent's task using the agentic tool-call loop.
    async fn execute_agent_task(
        &self,
        name: &str,
        agent_config: &DelegateAgentConfig,
        full_prompt: &str,
    ) -> anyhow::Result<String> {
        // Create provider
        let provider_credential_owned = agent_config
            .api_key
            .clone()
            .or_else(|| self.fallback_credential.clone());
        #[allow(clippy::option_as_ref_deref)]
        let provider_credential = provider_credential_owned.as_ref().map(String::as_str);

        let provider: Box<dyn Provider> = providers::create_provider_with_options(
            &agent_config.provider,
            provider_credential,
            &self.provider_runtime_options,
        )?;

        // Filter parent tools by allowlist, excluding delegate and spawn_agent
        let allowed = agent_config
            .allowed_tools
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .collect::<std::collections::HashSet<_>>();

        let sub_tools: Vec<Box<dyn Tool>> = {
            let parent_tools = self.parent_tools.read();
            parent_tools
                .iter()
                .filter(|tool| allowed.contains(tool.name()))
                .filter(|tool| tool.name() != "delegate" && tool.name() != "spawn_agent")
                .map(|tool| Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>)
                .collect()
        };

        if sub_tools.is_empty() {
            anyhow::bail!(
                "Spawned agent '{name}' has no executable tools after filtering allowlist ({})",
                agent_config.allowed_tools.join(", ")
            );
        }

        // Build enriched system prompt
        let enriched_system_prompt =
            self.build_enriched_system_prompt(agent_config, &sub_tools, &self.workspace_dir);

        let mut history = Vec::new();
        if let Some(system_prompt) = enriched_system_prompt.as_ref() {
            history.push(ChatMessage::system(system_prompt.clone()));
        }
        history.push(ChatMessage::user(full_prompt.to_string()));

        let noop_observer = NoopObserver;
        let temperature = agent_config.temperature.unwrap_or(0.7);

        let agentic_timeout_secs = agent_config
            .agentic_timeout_secs
            .unwrap_or(self.delegate_config.agentic_timeout_secs);

        let result = tokio::time::timeout(
            Duration::from_secs(agentic_timeout_secs),
            run_tool_call_loop(
                &*provider,
                &mut history,
                &sub_tools,
                &noop_observer,
                &agent_config.provider,
                &agent_config.model,
                temperature,
                true, // silent
                None, // approval
                "spawn_agent",
                None, // channel_reply_target
                &self.multimodal_config,
                agent_config.max_iterations,
                None, // cancellation_token
                None, // on_delta
                None, // hooks
                &[],  // excluded_tools
                &[],  // dedup_exempt_tools
                None, // activated_tools
                None, // model_switch_callback
                &crate::config::PacingConfig::default(),
                agent_config.max_tool_result_chars.unwrap_or(0),
                agent_config.max_context_tokens.unwrap_or(0),
                None, // shared_budget
            ),
        )
        .await;

        match result {
            Ok(Ok(response)) => {
                if response.trim().is_empty() {
                    Ok("[Empty response]".to_string())
                } else {
                    Ok(response)
                }
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                anyhow::bail!("Spawned agent '{name}' timed out after {agentic_timeout_secs}s")
            }
        }
    }

    /// Persist agent configuration to `workspace/agents/<name>/` for reuse.
    async fn save_agent_to_workspace(
        &self,
        name: &str,
        config: &DelegateAgentConfig,
    ) -> anyhow::Result<()> {
        let agent_dir = self.workspace_dir.join("agents").join(name);
        tokio::fs::create_dir_all(&agent_dir).await?;

        // Write config.toml (without system_prompt - that goes to IDENTITY.md)
        let toml_content = format!(
            "provider = {:?}\n\
             model = {:?}\n\
             agentic = {}\n\
             max_iterations = {}\n\
             max_depth = {}\n\
             allowed_tools = {:?}\n",
            config.provider,
            config.model,
            config.agentic,
            config.max_iterations,
            config.max_depth,
            config.allowed_tools,
        );
        tokio::fs::write(agent_dir.join("config.toml"), toml_content).await?;

        if let Some(prompt) = &config.system_prompt {
            tokio::fs::write(agent_dir.join("IDENTITY.md"), prompt).await?;
        }

        info!(agent = name, dir = %agent_dir.display(), "Saved spawned agent to workspace");
        Ok(())
    }
}

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Create and run an ad-hoc sub-agent with a custom persona and tool set. \
         The agent runs with its own isolated context. Use for specialized tasks \
         that need a dedicated agent without pre-configuration."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["name", "system_prompt", "prompt"],
            "additionalProperties": false,
            "properties": {
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Unique identifier for this agent"
                },
                "system_prompt": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The agent's persona, role, and instructions"
                },
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task to execute"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider override (e.g. 'openai', 'anthropic', 'ollama'). Falls back to default_provider."
                },
                "model": {
                    "type": "string",
                    "description": "Model override (e.g. 'claude-sonnet-4-20250514'). Falls back to default_model."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tool allowlist for the agent. If empty, uses a safe default set \
                                    (file_read, memory_recall, web_search, web_fetch, calculator)."
                },
                "context": {
                    "type": "string",
                    "description": "Optional context to prepend to the prompt"
                },
                "mode": {
                    "type": "string",
                    "enum": ["sync", "background"],
                    "default": "sync",
                    "description": "Execution mode. 'background' returns a task_id immediately."
                },
                "save": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, persist this agent to workspace/agents/<name>/ for reuse across restarts."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security check
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "spawn_agent")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Parse required parameters
        let name = match args.get("name").and_then(|v| v.as_str()).map(str::trim) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'name' parameter".into()),
                });
            }
        };

        let system_prompt = match args
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
        {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'system_prompt' parameter".into()),
                });
            }
        };

        let prompt = match args.get("prompt").and_then(|v| v.as_str()).map(str::trim) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'prompt' parameter".into()),
                });
            }
        };

        // Parse optional parameters
        let provider = args
            .get("provider")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let allowed_tools = args
            .get("allowed_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<String>>()
            })
            .filter(|v| !v.is_empty());

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");

        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("sync");

        let save = args.get("save").and_then(|v| v.as_bool()).unwrap_or(false);

        // Build the DelegateAgentConfig
        let agent_config = DelegateAgentConfig {
            provider: provider.unwrap_or_else(|| self.default_provider.clone()),
            model: model.unwrap_or_else(|| self.default_model.clone()),
            system_prompt: Some(system_prompt),
            api_key: self.fallback_credential.clone(),
            temperature: None,
            max_depth: 3,
            agentic: true,
            allowed_tools: allowed_tools.unwrap_or_else(|| {
                vec![
                    "file_read".into(),
                    "memory_recall".into(),
                    "web_search".into(),
                    "web_fetch".into(),
                    "calculator".into(),
                ]
            }),
            max_iterations: 10,
            timeout_secs: None,
            agentic_timeout_secs: None,
            skills_directory: None,
            memory_namespace: Some(name.clone()),
            max_context_tokens: None,
            max_tool_result_chars: None,
        };

        // Register in shared agents map
        {
            let mut agents = self.agents.write();
            agents.insert(name.clone(), agent_config.clone());
        }

        info!(
            agent = %name,
            model = %agent_config.model,
            tools = ?agent_config.allowed_tools,
            mode = mode,
            "Spawning ad-hoc agent"
        );

        // Acquire concurrency permit if semaphore is configured.
        let _permit = match &self.subagent_semaphore {
            Some(sem) => Some(
                sem.acquire()
                    .await
                    .map_err(|_| anyhow::anyhow!("subagent semaphore closed"))?,
            ),
            None => None,
        };

        // Build full prompt with optional context
        let full_prompt = if context.is_empty() {
            prompt.clone()
        } else {
            format!("[Context]\n{context}\n\n[Task]\n{prompt}")
        };

        if mode == "background" {
            return self
                .execute_background(&name, &agent_config, &full_prompt, save)
                .await;
        }

        // Synchronous execution
        let result = self
            .execute_agent_task(&name, &agent_config, &full_prompt)
            .await;

        // Save if requested (best-effort, don't fail the tool call)
        if save {
            if let Err(e) = self.save_agent_to_workspace(&name, &agent_config).await {
                warn!(agent = %name, error = %e, "Failed to save spawned agent to workspace");
            }
        }

        match result {
            Ok(response) => Ok(ToolResult {
                success: true,
                output: format!(
                    "[Spawned agent '{name}' ({provider}/{model}, agentic)]\n{response}",
                    provider = agent_config.provider,
                    model = agent_config.model,
                ),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Spawned agent '{name}' failed: {e}")),
            }),
        }
    }
}

impl SpawnAgentTool {
    /// Spawn the agent in a background tokio task. Returns a task_id immediately.
    async fn execute_background(
        &self,
        name: &str,
        agent_config: &DelegateAgentConfig,
        full_prompt: &str,
        save: bool,
    ) -> anyhow::Result<ToolResult> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let results_dir = self.results_dir();
        tokio::fs::create_dir_all(&results_dir).await?;

        let started_at = chrono::Utc::now().to_rfc3339();
        let name_owned = name.to_string();

        // Write initial "running" status
        let initial_result = BackgroundDelegateResult {
            task_id: task_id.clone(),
            agent: name_owned.clone(),
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
        let fallback_credential = self.fallback_credential.clone();
        let provider_runtime_options = self.provider_runtime_options.clone();
        let parent_tools = Arc::clone(&self.parent_tools);
        let multimodal_config = self.multimodal_config.clone();
        let delegate_config = self.delegate_config.clone();
        let workspace_dir = self.workspace_dir.clone();
        let skills_prompt_mode = self.skills_prompt_mode;
        let memory = self.memory.clone();
        let default_provider = self.default_provider.clone();
        let default_model = self.default_model.clone();
        let agent_config_clone = agent_config.clone();
        let full_prompt_owned = full_prompt.to_string();
        let task_id_clone = task_id.clone();

        tokio::spawn(async move {
            // Build an inner SpawnAgentTool for the spawned context
            let inner = SpawnAgentTool {
                agents,
                security,
                fallback_credential,
                provider_runtime_options,
                parent_tools,
                multimodal_config,
                delegate_config,
                workspace_dir: workspace_dir.clone(),
                skills_prompt_mode,
                memory,
                default_provider,
                default_model,
                subagent_semaphore: None,
            };

            let outcome = inner
                .execute_agent_task(&name_owned, &agent_config_clone, &full_prompt_owned)
                .await;

            // Save if requested (best-effort)
            if save {
                if let Err(e) = inner
                    .save_agent_to_workspace(&name_owned, &agent_config_clone)
                    .await
                {
                    warn!(
                        agent = %name_owned,
                        error = %e,
                        "Failed to save spawned agent to workspace (background)"
                    );
                }
            }

            let finished_at = chrono::Utc::now().to_rfc3339();
            let final_result = match outcome {
                Ok(output) => BackgroundDelegateResult {
                    task_id: task_id_clone.clone(),
                    agent: name_owned,
                    status: BackgroundTaskStatus::Completed,
                    output: Some(output),
                    error: None,
                    started_at,
                    finished_at: Some(finished_at),
                },
                Err(err) => BackgroundDelegateResult {
                    task_id: task_id_clone.clone(),
                    agent: name_owned,
                    status: BackgroundTaskStatus::Failed,
                    output: None,
                    error: Some(err.to_string()),
                    started_at,
                    finished_at: Some(finished_at),
                },
            };

            let result_path = results_dir.join(format!("{}.json", task_id_clone));
            if let Ok(bytes) = serde_json::to_vec_pretty(&final_result) {
                let _ = tokio::fs::write(&result_path, &bytes).await;
            }
        });

        Ok(ToolResult {
            success: true,
            output: format!(
                "Background task started for spawned agent '{name}'.\n\
                 task_id: {task_id}\n\
                 Use delegate tool with action='check_result' and task_id='{task_id}' to retrieve the result."
            ),
            error: None,
        })
    }
}

// ── Helper types ──────────────────────────────────────────────────

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
