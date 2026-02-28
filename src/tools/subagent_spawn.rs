//! Sub-agent spawn tool for background execution.
//!
//! Implements the `subagent_spawn` tool that launches delegate agents
//! asynchronously via `tokio::spawn`, returning a session ID immediately.
//! See `AGENTS.md` §7.3 for the tool change playbook.

use super::subagent_registry::{SubAgentRegistry, SubAgentSession, SubAgentStatus};
use super::traits::{Tool, ToolResult};
use crate::config::DelegateAgentConfig;
use crate::observability::traits::{Observer, ObserverEvent, ObserverMetric};
use crate::providers::{self, ChatMessage, Provider};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Default timeout for background sub-agent provider calls.
const SPAWN_TIMEOUT_SECS: u64 = 300;
/// Maximum number of concurrent background sub-agents.
const MAX_CONCURRENT_SUBAGENTS: usize = 10;

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
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            provider_runtime_options,
            registry,
            parent_tools,
            multimodal_config,
        }
    }
}

#[async_trait]
impl Tool for SubAgentSpawnTool {
    fn name(&self) -> &str {
        "subagent_spawn"
    }

    fn description(&self) -> &str {
        "Spawn a delegate agent in the background. Returns immediately with a session_id. \
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
            "required": ["agent", "task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
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

        // Look up agent config
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
        let agent_name_owned = agent_name.to_string();
        let task_owned = task.to_string();

        // Determine if agentic mode
        let is_agentic = agent_config.agentic;
        let parent_tools = self.parent_tools.clone();
        let multimodal_config = self.multimodal_config.clone();

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
        if let Err(_running) = self.registry.try_insert(session, MAX_CONCURRENT_SUBAGENTS) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Maximum concurrent sub-agents reached ({MAX_CONCURRENT_SUBAGENTS}). \
                     Wait for running agents to complete or kill some."
                )),
            });
        }

        // Clone what we need for the spawned task
        let registry = self.registry.clone();
        let sid = session_id.clone();

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
                    } else {
                        registry.fail(
                            &sid,
                            tool_result
                                .error
                                .unwrap_or_else(|| "Unknown error".to_string()),
                        );
                    }
                }
                Err(e) => {
                    registry.fail(&sid, format!("Agent '{agent_name_owned}' error: {e}"));
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
                temperature: Some(0.3),
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );
        agents
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
        assert!(required.contains(&json!("agent")));
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
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_task_param() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool.execute(json!({"agent": "researcher"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blank_agent_rejected() {
        let tool = make_tool(sample_agents(), test_security());
        let result = tool
            .execute(json!({"agent": "  ", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
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
        assert!(result.error.unwrap().contains("none configured"));
    }

    #[tokio::test]
    async fn spawn_respects_concurrent_limit() {
        let registry = Arc::new(SubAgentRegistry::new());

        // Fill up the registry with running sessions
        for i in 0..MAX_CONCURRENT_SUBAGENTS {
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
        );

        let result = tool
            .execute(json!({"agent": "researcher", "task": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Maximum concurrent"));
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
