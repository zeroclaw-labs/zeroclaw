//! Sub-agent spawn tool for background execution.
//!
//! Implements the `subagent_spawn` tool that launches delegate agents
//! asynchronously via `tokio::spawn`, returning a session ID immediately.
//! See `AGENTS.md` §7.3 for the tool change playbook.

use super::agent_load_tracker::AgentLoadTracker;
use super::agent_selection::{select_agent_with_load, AgentSelectionPolicy};
use super::orchestration_settings::load_orchestration_settings;
use super::subagent_registry::{SubAgentRegistry, SubAgentSession, SubAgentStatus};
use super::traits::{Tool, ToolResult};
use crate::config::{DelegateAgentConfig, SubAgentsConfig};
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::providers::{self, ChatMessage, Provider};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Default timeout for background sub-agent provider calls.
const SPAWN_TIMEOUT_SECS: u64 = 300;

/// Tool that spawns a delegate agent in the background, returning immediately
/// with a session ID. The sub-agent runs asynchronously and stores its result
/// in the shared [`SubAgentRegistry`].
pub struct SubAgentSpawnTool {
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    security: Arc<SecurityPolicy>,
    fallback_credential: Option<String>,
    provider_runtime_options: providers::ProviderRuntimeOptions,
    registry: Arc<SubAgentRegistry>,
    parent_tools: Arc<Vec<Arc<dyn Tool>>>,
    multimodal_config: crate::config::MultimodalConfig,
    subagent_settings: SubAgentsConfig,
    load_tracker: AgentLoadTracker,
    runtime_config_path: Option<PathBuf>,
}

impl SubAgentSpawnTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        provider_runtime_options: providers::ProviderRuntimeOptions,
        registry: Arc<SubAgentRegistry>,
        parent_tools: Arc<Vec<Arc<dyn Tool>>>,
        multimodal_config: crate::config::MultimodalConfig,
        subagents_enabled: bool,
        max_concurrent_subagents: usize,
        auto_activate: bool,
        runtime_config_path: Option<PathBuf>,
    ) -> Self {
        let mut subagent_settings = SubAgentsConfig::default();
        subagent_settings.enabled = subagents_enabled;
        subagent_settings.max_concurrent = max_concurrent_subagents.max(1);
        subagent_settings.auto_activate = auto_activate;

        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            provider_runtime_options,
            registry,
            parent_tools,
            multimodal_config,
            subagent_settings,
            load_tracker: AgentLoadTracker::new(),
            runtime_config_path,
        }
    }

    /// Reuse a shared runtime load tracker.
    pub fn with_load_tracker(mut self, load_tracker: AgentLoadTracker) -> Self {
        self.load_tracker = load_tracker;
        self
    }

    fn runtime_subagent_settings(&self) -> SubAgentsConfig {
        let mut settings = self.subagent_settings.clone();
        settings.max_concurrent = settings.max_concurrent.max(1);
        settings.load_window_secs = settings.load_window_secs.max(1);
        settings.queue_poll_ms = settings.queue_poll_ms.max(1);

        if let Some(path) = self.runtime_config_path.as_deref() {
            match load_orchestration_settings(path) {
                Ok((_teams, subagents)) => {
                    settings = subagents;
                    settings.max_concurrent = settings.max_concurrent.max(1);
                    settings.load_window_secs = settings.load_window_secs.max(1);
                    settings.queue_poll_ms = settings.queue_poll_ms.max(1);
                }
                Err(error) => {
                    tracing::debug!(
                        path = %path.display(),
                        "subagent_spawn: failed to hot-reload orchestration settings: {error}"
                    );
                }
            }
        }

        settings
    }

    async fn wait_for_slot_and_insert(
        &self,
        mut session: SubAgentSession,
        settings: &SubAgentsConfig,
    ) -> Result<(), usize> {
        let max_concurrent = settings.max_concurrent.max(1);
        match self.registry.try_insert(session, max_concurrent) {
            Ok(()) => return Ok(()),
            Err((running, returned)) => {
                if settings.queue_wait_ms == 0 {
                    return Err(running);
                }
                session = returned;
            }
        }

        let poll_ms = settings.queue_poll_ms.max(1);
        let wait_deadline = tokio::time::Instant::now()
            + Duration::from_millis(u64::try_from(settings.queue_wait_ms).unwrap_or(u64::MAX));
        let poll_duration = Duration::from_millis(u64::try_from(poll_ms).unwrap_or(1));
        let mut last_running = self.registry.running_count();

        while tokio::time::Instant::now() < wait_deadline {
            tokio::time::sleep(poll_duration).await;
            match self.registry.try_insert(session, max_concurrent) {
                Ok(()) => return Ok(()),
                Err((running, returned)) => {
                    last_running = running;
                    session = returned;
                }
            }
        }

        Err(last_running)
    }
}

#[async_trait]
impl Tool for SubAgentSpawnTool {
    fn name(&self) -> &str {
        "subagent_spawn"
    }

    fn description(&self) -> &str {
        "Spawn a delegate agent in the background. Returns immediately with a session_id. \
         `agent` can be omitted or set to `auto` when subagent auto-activation is enabled. \
         Use subagent_list to check progress and subagent_manage to steer or kill."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let agent_names: Vec<&str> = self.agents.keys().map(|s: &String| s.as_str()).collect();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "agent": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!(
                        "Name of the agent to spawn. Available: {}",
                        if agent_names.is_empty() {
                            "(none configured)".to_string()
                        } else {
                            agent_names.join(", ")
                        }
                    )
                },
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt to send to the sub-agent"
                },
                "context": {
                    "type": "string",
                    "description": "Optional context to prepend (e.g. relevant code, prior findings)"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let requested_agent = args.get("agent").and_then(|v| v.as_str()).map(str::trim);

        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'task' parameter"))?;

        if task.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'task' parameter must not be empty".into()),
            });
        }

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");

        let subagent_settings = self.runtime_subagent_settings();
        if !subagent_settings.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Subagents are currently disabled. Re-enable with model_routing_config action set_orchestration."
                        .to_string(),
                ),
            });
        }

        // Security enforcement: spawn is a write operation
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "subagent_spawn")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let load_window_secs = u64::try_from(subagent_settings.load_window_secs).unwrap_or(1);
        let load_snapshot = self
            .load_tracker
            .snapshot(Duration::from_secs(load_window_secs.max(1)));
        let selection_policy = AgentSelectionPolicy {
            strategy: subagent_settings.strategy,
            inflight_penalty: subagent_settings.inflight_penalty,
            recent_selection_penalty: subagent_settings.recent_selection_penalty,
            recent_failure_penalty: subagent_settings.recent_failure_penalty,
        };

        let selection = match select_agent_with_load(
            self.agents.as_ref(),
            requested_agent,
            task,
            context,
            subagent_settings.auto_activate,
            None,
            Some(&load_snapshot),
            selection_policy,
        ) {
            Ok(selection) => selection,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error.to_string()),
                });
            }
        };
        let agent_name = selection.agent_name.clone();
        let Some(agent_config) = self.agents.get(&agent_name).cloned() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Resolved agent '{agent_name}' is unavailable")),
            });
        };

        // Create provider for this agent
        let provider_credential_owned = agent_config
            .api_key
            .clone()
            .or_else(|| self.fallback_credential.clone());
        #[allow(clippy::option_as_ref_deref)]
        let provider_credential = provider_credential_owned.as_ref().map(String::as_str);

        let provider: Box<dyn Provider> = match providers::create_provider_with_options(
            &agent_config.provider,
            provider_credential,
            &self.provider_runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => {
                self.load_tracker.record_failure(&agent_name);
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Failed to create provider '{}' for agent '{agent_name}': {e}",
                        agent_config.provider
                    )),
                });
            }
        };

        // Build the message
        let full_prompt = if context.is_empty() {
            task.to_string()
        } else {
            format!("[Context]\n{context}\n\n[Task]\n{task}")
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        let agent_name_owned = agent_name.clone();
        let task_owned = task.to_string();

        // Determine if agentic mode
        let is_agentic = agent_config.agentic;
        let parent_tools = self.parent_tools.clone();
        let multimodal_config = self.multimodal_config.clone();
        let mut load_lease = self.load_tracker.start(&agent_name_owned);

        // Atomically check concurrent limit and register session to prevent race conditions.
        let session = SubAgentSession {
            id: session_id.clone(),
            agent_name: agent_name_owned.clone(),
            task: task_owned,
            status: SubAgentStatus::Running,
            started_at: Utc::now(),
            completed_at: None,
            result: None,
            handle: None,
        };
        if let Err(running) = self
            .wait_for_slot_and_insert(session, &subagent_settings)
            .await
        {
            load_lease.mark_failure();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Maximum concurrent sub-agents reached ({limit}), currently running {running}. \
                     Wait for running agents to complete, tune queue_wait_ms/queue_poll_ms, or kill some.",
                    limit = subagent_settings.max_concurrent
                )),
            });
        }

        // Clone what we need for the spawned task
        let registry = self.registry.clone();
        let sid = session_id.clone();
        let mut bg_load_lease = load_lease;

        let handle = tokio::spawn(async move {
            let result = if is_agentic {
                run_agentic_background(
                    &agent_name_owned,
                    &agent_config,
                    &*provider,
                    &full_prompt,
                    &parent_tools,
                    &multimodal_config,
                )
                .await
            } else {
                run_simple_background(&agent_name_owned, &agent_config, &*provider, &full_prompt)
                    .await
            };

            match result {
                Ok(tool_result) => {
                    if tool_result.success {
                        registry.complete(&sid, tool_result);
                        bg_load_lease.mark_success();
                    } else {
                        registry.fail(
                            &sid,
                            tool_result
                                .error
                                .unwrap_or_else(|| "Unknown error".to_string()),
                        );
                        bg_load_lease.mark_failure();
                    }
                }
                Err(e) => {
                    registry.fail(&sid, format!("Agent '{agent_name_owned}' error: {e}"));
                    bg_load_lease.mark_failure();
                }
            }
        });

        // Store the handle for cancellation
        self.registry.set_handle(&session_id, handle);

        Ok(ToolResult {
            success: true,
            output: json!({
                "session_id": session_id,
                "agent": agent_name,
                "selection_mode": selection.selection_mode,
                "selection_score": selection.score,
                "max_concurrent": subagent_settings.max_concurrent,
                "queue_wait_ms": subagent_settings.queue_wait_ms,
                "queue_poll_ms": subagent_settings.queue_poll_ms,
                "status": "running",
                "message": "Sub-agent spawned in background. Use subagent_list or subagent_manage to check progress."
            })
            .to_string(),
            error: None,
        })
    }
}

async fn run_simple_background(
    agent_name: &str,
    agent_config: &DelegateAgentConfig,
    provider: &dyn Provider,
    full_prompt: &str,
) -> anyhow::Result<ToolResult> {
    let temperature = agent_config.temperature.unwrap_or(0.7);

    let result = tokio::time::timeout(
        Duration::from_secs(SPAWN_TIMEOUT_SECS),
        provider.chat_with_system(
            agent_config.system_prompt.as_deref(),
            full_prompt,
            &agent_config.model,
            temperature,
        ),
    )
    .await;

    let result = match result {
        Ok(inner) => inner,
        Err(_elapsed) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' timed out after {SPAWN_TIMEOUT_SECS}s"
                )),
            });
        }
    };

    match result {
        Ok(response) => {
            let rendered = if response.trim().is_empty() {
                "[Empty response]".to_string()
            } else {
                response
            };

            Ok(ToolResult {
                success: true,
                output: format!(
                    "[Agent '{agent_name}' ({provider}/{model})]\n{rendered}",
                    provider = agent_config.provider,
                    model = agent_config.model
                ),
                error: None,
            })
        }
        Err(e) => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Agent '{agent_name}' failed: {e}")),
        }),
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

async fn run_agentic_background(
    agent_name: &str,
    agent_config: &DelegateAgentConfig,
    provider: &dyn Provider,
    full_prompt: &str,
    parent_tools: &[Arc<dyn Tool>],
    multimodal_config: &crate::config::MultimodalConfig,
) -> anyhow::Result<ToolResult> {
    if agent_config.allowed_tools.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Agent '{agent_name}' has agentic=true but allowed_tools is empty"
            )),
        });
    }

    let allowed = agent_config
        .allowed_tools
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<std::collections::HashSet<_>>();

    let sub_tools: Vec<Box<dyn Tool>> = parent_tools
        .iter()
        .filter(|tool| allowed.contains(tool.name()))
        .filter(|tool| {
            tool.name() != "delegate"
                && tool.name() != "subagent_spawn"
                && tool.name() != "subagent_manage"
        })
        .map(|tool| Box::new(ToolArcRef::new(tool.clone())) as Box<dyn Tool>)
        .collect();

    if sub_tools.is_empty() {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Agent '{agent_name}' has no executable tools after filtering allowlist ({})",
                agent_config.allowed_tools.join(", ")
            )),
        });
    }

    let temperature = agent_config.temperature.unwrap_or(0.7);
    let mut history = Vec::new();
    if let Some(system_prompt) = agent_config.system_prompt.as_ref() {
        history.push(ChatMessage::system(system_prompt.clone()));
    }
    history.push(ChatMessage::user(full_prompt.to_string()));

    let noop_observer = NoopObserver;

    let result = tokio::time::timeout(
        Duration::from_secs(SPAWN_TIMEOUT_SECS),
        crate::agent::loop_::run_tool_call_loop(
            provider,
            &mut history,
            &sub_tools,
            &noop_observer,
            &agent_config.provider,
            &agent_config.model,
            temperature,
            true,
            None,
            "subagent_spawn",
            multimodal_config,
            agent_config.max_iterations,
            None,
            None,
            None,
            &[],
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
                    "[Agent '{agent_name}' ({provider}/{model}, agentic)]\n{rendered}",
                    provider = agent_config.provider,
                    model = agent_config.model
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
                "Agent '{agent_name}' timed out after {SPAWN_TIMEOUT_SECS}s"
            )),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn sample_agents() -> HashMap<String, DelegateAgentConfig> {
        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: Some("You are a research assistant.".to_string()),
                api_key: None,
                enabled: true,
                capabilities: vec!["research".to_string()],
                priority: 0,
                temperature: Some(0.3),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        agents
    }

    fn write_runtime_orchestration_config(
        path: &std::path::Path,
        teams_enabled: bool,
        teams_auto_activate: bool,
        teams_max_agents: usize,
        subagents_enabled: bool,
        subagents_auto_activate: bool,
        subagents_max_concurrent: usize,
    ) {
        let contents = format!(
            r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7

[agent.teams]
enabled = {teams_enabled}
auto_activate = {teams_auto_activate}
max_agents = {teams_max_agents}

[agent.subagents]
enabled = {subagents_enabled}
auto_activate = {subagents_auto_activate}
max_concurrent = {subagents_max_concurrent}
queue_wait_ms = 0
queue_poll_ms = 10
"#
        );
        std::fs::write(path, contents).unwrap();
    }

    fn make_tool(
        agents: HashMap<String, DelegateAgentConfig>,
        security: Arc<SecurityPolicy>,
    ) -> SubAgentSpawnTool {
        SubAgentSpawnTool::new(
            agents,
            None,
            security,
            providers::ProviderRuntimeOptions::default(),
            Arc::new(SubAgentRegistry::new()),
            Arc::new(Vec::new()),
            crate::config::MultimodalConfig::default(),
            true,
            10,
            true,
            None,
        )
    }

    #[test]
    fn name_and_schema() {
        let tool = make_tool(sample_agents(), test_security());
        assert_eq!(tool.name(), "subagent_spawn");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["task"].is_object());
        assert!(schema["properties"]["context"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("task")));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn description_not_empty() {
        let tool = make_tool(sample_agents(), test_security());
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn missing_agent_param() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool.execute(json!({"task": "test"})).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn missing_task_param() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool.execute(json!({"agent": "researcher"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blank_agent_uses_auto_selection() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool
            .execute(json!({"agent": "  ", "task": "test"}))
            .await
            .unwrap();
        if result.success {
            let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            assert_eq!(output["agent"], json!("researcher"));
            assert!(output["selection_mode"].is_string());
        }
    }

    #[tokio::test]
    async fn blank_task_rejected() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool
            .execute(json!({"agent": "researcher", "task": "  "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn unknown_agent_returns_error() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool
            .execute(json!({"agent": "nonexistent", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));
    }

    #[tokio::test]
    async fn spawn_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = make_tool(sample_agents(), readonly);
        let result = tool
            .execute(json!({"agent": "researcher", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
    }

    #[tokio::test]
    async fn spawn_blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = make_tool(sample_agents(), limited);
        let result = tool
            .execute(json!({"agent": "researcher", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
    }

    #[tokio::test]
    async fn runtime_subagent_disable_blocks_spawn() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        write_runtime_orchestration_config(&config_path, true, true, 8, false, true, 2);

        let tool = SubAgentSpawnTool::new(
            sample_agents(),
            None,
            test_security(),
            providers::ProviderRuntimeOptions::default(),
            Arc::new(SubAgentRegistry::new()),
            Arc::new(Vec::new()),
            crate::config::MultimodalConfig::default(),
            true,
            10,
            true,
            Some(config_path),
        );

        let result = tool
            .execute(json!({"agent": "researcher", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("Subagents are currently disabled"));
    }

    #[tokio::test]
    async fn runtime_subagent_auto_activation_disable_requires_agent() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        write_runtime_orchestration_config(&config_path, true, true, 8, true, false, 2);

        let tool = SubAgentSpawnTool::new(
            sample_agents(),
            None,
            test_security(),
            providers::ProviderRuntimeOptions::default(),
            Arc::new(SubAgentRegistry::new()),
            Arc::new(Vec::new()),
            crate::config::MultimodalConfig::default(),
            true,
            10,
            true,
            Some(config_path),
        );

        let result = tool.execute(json!({"task": "test"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("automatic activation is disabled"));
    }

    #[tokio::test]
    async fn spawn_returns_session_id() {
        // The agent has an invalid provider so the background task will fail,
        // but spawn itself returns immediately with a session_id.
        let tool = make_tool(sample_agents(), test_security());
        let result = tool
            .execute(json!({"agent": "researcher", "task": "test task"}))
            .await
            .unwrap();
        // Spawn may fail at provider creation if the provider is invalid
        // For ollama, it should successfully create the provider even without a running server
        // The result could succeed (spawn) or fail (invalid provider) depending on environment
        if result.success {
            let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            assert!(output["session_id"].is_string());
            assert_eq!(output["status"], "running");
        }
        // Either way, no panic
    }

    #[tokio::test]
    async fn spawn_no_agents_configured() {
        let tool = make_tool(HashMap::new(), test_security());
        let result = tool
            .execute(json!({"agent": "any", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("No delegate agents are configured"));
    }

    #[tokio::test]
    async fn spawn_respects_concurrent_limit() {
        let max_concurrent = 3usize;
        let registry = Arc::new(SubAgentRegistry::new());
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        write_runtime_orchestration_config(&config_path, true, true, 8, true, true, max_concurrent);

        // Fill up the registry with running sessions
        for i in 0..max_concurrent {
            registry.insert(SubAgentSession {
                id: format!("s{i}"),
                agent_name: "agent".to_string(),
                task: "task".to_string(),
                status: SubAgentStatus::Running,
                started_at: Utc::now(),
                completed_at: None,
                result: None,
                handle: None,
            });
        }

        let tool = SubAgentSpawnTool::new(
            sample_agents(),
            None,
            test_security(),
            providers::ProviderRuntimeOptions::default(),
            registry,
            Arc::new(Vec::new()),
            crate::config::MultimodalConfig::default(),
            true,
            max_concurrent,
            true,
            Some(config_path),
        );

        let result = tool
            .execute(json!({"agent": "researcher", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Maximum concurrent"));
    }

    #[tokio::test]
    async fn runtime_max_concurrent_override_is_applied() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        write_runtime_orchestration_config(&config_path, true, true, 8, true, true, 1);

        let registry = Arc::new(SubAgentRegistry::new());
        registry.insert(SubAgentSession {
            id: "existing".to_string(),
            agent_name: "researcher".to_string(),
            task: "task".to_string(),
            status: SubAgentStatus::Running,
            started_at: Utc::now(),
            completed_at: None,
            result: None,
            handle: None,
        });

        let tool = SubAgentSpawnTool::new(
            sample_agents(),
            None,
            test_security(),
            providers::ProviderRuntimeOptions::default(),
            registry,
            Arc::new(Vec::new()),
            crate::config::MultimodalConfig::default(),
            true,
            10,
            true,
            Some(config_path),
        );

        let result = tool
            .execute(json!({"agent": "researcher", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("Maximum concurrent sub-agents reached (1)"));
    }

    #[tokio::test]
    async fn schema_lists_agent_names() {
        let tool = make_tool(sample_agents(), test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["agent"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher"));
    }
}
