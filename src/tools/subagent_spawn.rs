use super::traits::{Tool, ToolResult};
use crate::agent::loop_::run_tool_call_loop;
use crate::agent::subagent_registry::{SubagentOutcome, SubagentRegistry, SubagentRunRecord};
use crate::config::DelegateAgentConfig;
use crate::observability::noop::NoopObserver;
use crate::providers::{
    self, reliable::ReliableProvider, ChatMessage, Provider, ProviderRuntimeOptions,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Default run timeout for spawned subagents.
///
/// `0` means no timeout (OpenClaw-style behavior).
const DEFAULT_SPAWN_RUN_TIMEOUT_SECS: u64 = 0;

/// Extra execution constraints automatically applied to the `dev` agent.
const DEV_AGENT_RUNTIME_RULES: &str = "[Runtime Execution Rules]\n- Never run local dev servers in foreground. Use background mode (`nohup ... > logs/dev/<name>.log 2>&1 &`) and record PID.\n- After starting background services, run only short readiness checks (for example `curl --max-time 5 ...`) and continue immediately.\n- For searches, always scope by path and glob/include filters. Avoid repo-wide wildcard scans.\n- Prefer narrow iterative searches with bounded result counts instead of one broad sweep.";

fn parse_run_timeout_seconds(args: &Value) -> anyhow::Result<Option<u64>> {
    let Some(raw) = args
        .get("run_timeout_seconds")
        .or_else(|| args.get("timeout_seconds"))
    else {
        return Ok(None);
    };

    let value = raw
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("'run_timeout_seconds' must be a non-negative integer"))?;
    Ok(Some(value))
}

fn compose_subagent_system_prompt(
    agent_name: Option<&str>,
    base_prompt: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(base) = base_prompt.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(base.to_string());
    }

    if agent_name
        .map(|name| name.eq_ignore_ascii_case("dev"))
        .unwrap_or(false)
    {
        parts.push(DEV_AGENT_RUNTIME_RULES.to_string());
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Tool that spawns a background subagent to execute a task asynchronously.
/// Always returns immediately with a run ID. Completion results are
/// automatically announced back to the parent conversation via push-based
/// messaging (no polling needed).
pub struct SubagentSpawnTool {
    registry: Arc<SubagentRegistry>,
    provider_runtime_options: ProviderRuntimeOptions,
    fallback_credential: Option<String>,
    default_provider: String,
    /// Named agent configurations (dev, qa, reporter, etc.)
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    /// Parent tool registry for agentic subagents
    parent_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// Multimodal config inherited from root
    multimodal_config: crate::config::MultimodalConfig,
}

impl SubagentSpawnTool {
    pub fn new(
        registry: Arc<SubagentRegistry>,
        provider_runtime_options: ProviderRuntimeOptions,
        fallback_credential: Option<String>,
        default_provider: String,
    ) -> Self {
        Self {
            registry,
            provider_runtime_options,
            fallback_credential,
            default_provider,
            agents: Arc::new(HashMap::new()),
            parent_tools: Arc::new(Vec::new()),
            multimodal_config: crate::config::MultimodalConfig::default(),
        }
    }

    pub fn with_agents(mut self, agents: Arc<HashMap<String, DelegateAgentConfig>>) -> Self {
        self.agents = agents;
        self
    }

    pub fn with_parent_tools(mut self, tools: Arc<Vec<Arc<dyn Tool>>>) -> Self {
        self.parent_tools = tools;
        self
    }

    pub fn with_multimodal_config(mut self, config: crate::config::MultimodalConfig) -> Self {
        self.multimodal_config = config;
        self
    }
}

#[async_trait]
impl Tool for SubagentSpawnTool {
    fn name(&self) -> &str {
        "sessions_spawn"
    }

    fn description(&self) -> &str {
        "Spawn a background subagent to work on a task asynchronously. Always returns immediately \
         with a run_id. The subagent result will be automatically announced back to you as a new \
         message when complete — no polling needed. Use 'subagents' tool to list/kill/steer active agents."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let agent_names: Vec<String> = self.agents.keys().cloned().collect();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt to send to the subagent"
                },
                "agent": {
                    "type": "string",
                    "description": format!(
                        "Named agent to use (e.g. {}). Each agent has its own system prompt, model, and tools. If omitted, runs a simple prompt.",
                        if agent_names.is_empty() { "none configured".to_string() }
                        else { agent_names.join(", ") }
                    )
                },
                "label": {
                    "type": "string",
                    "description": "Optional human-readable label for this subagent run"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override (e.g. 'gpt-5.4'). Uses agent's configured model by default."
                },
                "run_timeout_seconds": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional run timeout in seconds. `0` disables timeout."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Backward-compatible alias for `run_timeout_seconds`."
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
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

        let agent_name = args
            .get("agent")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| agent_name.clone());

        let model_override = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Resolve agent config if named
        let agent_config = agent_name
            .as_ref()
            .and_then(|name| self.agents.get(name))
            .cloned();

        if agent_name.is_some() && agent_config.is_none() {
            let available: Vec<&str> = self.agents.keys().map(|s| s.as_str()).collect();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown agent '{}'. Available agents: {}",
                    agent_name.unwrap(),
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )),
            });
        }

        if !self.registry.can_spawn().await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Maximum concurrent subagents reached. Wait for one to finish or cancel an existing subagent.".into(),
                ),
            });
        }

        // Determine provider/model from agent config or defaults
        let provider_name = agent_config
            .as_ref()
            .map(|c| c.provider.clone())
            .unwrap_or_else(|| self.default_provider.clone());
        let model = model_override.unwrap_or_else(|| {
            agent_config
                .as_ref()
                .map(|c| c.model.clone())
                .unwrap_or_else(|| "gpt-5.4".to_string())
        });
        let temperature = agent_config
            .as_ref()
            .and_then(|c| c.temperature)
            .unwrap_or(0.7);
        let base_system_prompt = agent_config
            .as_ref()
            .and_then(|c| c.system_prompt.as_deref());
        let effective_system_prompt =
            compose_subagent_system_prompt(agent_name.as_deref(), base_system_prompt);
        let is_agentic = agent_config.as_ref().is_some_and(|c| c.agentic);
        let max_iterations = agent_config
            .as_ref()
            .map(|c| c.max_iterations)
            .unwrap_or(24);
        let run_timeout_seconds = parse_run_timeout_seconds(&args)?
            .or_else(|| agent_config.as_ref().map(|c| c.run_timeout_seconds))
            .unwrap_or(DEFAULT_SPAWN_RUN_TIMEOUT_SECS);

        // Create provider wrapped in ReliableProvider for retry support.
        // Subagents disable incremental sends to avoid cross-session
        // `previous_response_not_found` race conditions.
        let mut subagent_options = self.provider_runtime_options.clone();
        subagent_options.disable_incremental = true;

        let provider: Box<dyn Provider> = match providers::create_provider_with_options(
            &provider_name,
            self.fallback_credential.as_deref(),
            &subagent_options,
        ) {
            Ok(p) => {
                let reliable = ReliableProvider::new(
                    vec![(provider_name.clone(), p)],
                    3,   // max retries
                    500, // base backoff ms
                );
                Box::new(reliable)
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to create provider '{provider_name}': {e}")),
                });
            }
        };

        // Build sub-tools for agentic mode
        let sub_tools: Vec<Arc<dyn Tool>> = if is_agentic {
            if let Some(ref config) = agent_config {
                let allowed: std::collections::HashSet<&str> = config
                    .allowed_tools
                    .iter()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect();

                self.parent_tools
                    .iter()
                    .filter(|tool| allowed.contains(tool.name()))
                    .filter(|tool| tool.name() != "sessions_spawn") // prevent recursive spawn
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let cancellation_token = CancellationToken::new();

        // Read parent context from registry (set by channel message processor)
        let parent_context = self.registry.current_context().await;

        let record = SubagentRunRecord {
            run_id: run_id.clone(),
            task: task.to_string(),
            label: label.clone(),
            model: model.clone(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: cancellation_token.clone(),
            parent_context,
        };

        self.registry.register(record).await;

        // Spawn background task
        let registry = self.registry.clone();
        let run_id_spawn = run_id.clone();
        let task_owned = task.to_string();
        let model_owned = model.clone();
        let provider_name_owned = provider_name.clone();
        let multimodal_config = self.multimodal_config.clone();
        let agent_label = label.clone().unwrap_or_else(|| "subagent".to_string());

        tokio::spawn(async move {
            let run_id = run_id_spawn;

            let result = if is_agentic && !sub_tools.is_empty() {
                // Full agentic loop with tools
                let boxed_tools: Vec<Box<dyn Tool>> = sub_tools
                    .into_iter()
                    .map(|t| Box::new(ToolArcRef::new(t)) as Box<dyn Tool>)
                    .collect();

                let mut history = Vec::new();
                if let Some(ref sp) = effective_system_prompt {
                    history.push(ChatMessage::system(sp.clone()));
                }
                history.push(ChatMessage::user(task_owned.clone()));

                let noop_observer = NoopObserver;

                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        Err("Cancelled".to_string())
                    }
                    res = async {
                        let agentic_loop = run_tool_call_loop(
                            &*provider,
                            &mut history,
                            &boxed_tools,
                            &noop_observer,
                            &provider_name_owned,
                            &model_owned,
                            temperature,
                            true,
                            None,
                            "sessions-spawn",
                            &multimodal_config,
                            max_iterations,
                            None,
                            None,
                            None,
                            &[],
                            None,
                        );

                        if run_timeout_seconds == 0 {
                            agentic_loop.await.map_err(|e| e.to_string())
                        } else {
                            match tokio::time::timeout(
                                Duration::from_secs(run_timeout_seconds),
                                agentic_loop,
                            )
                            .await
                            {
                                Ok(Ok(response)) => Ok(response),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(_) => Err(format!("Timed out after {run_timeout_seconds}s")),
                            }
                        }
                    } => res
                }
            } else {
                // Simple single-call mode
                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        Err("Cancelled".to_string())
                    }
                    res = async {
                        let simple_call = provider.chat_with_system(
                            effective_system_prompt.as_deref(),
                            &task_owned,
                            &model_owned,
                            temperature,
                        );

                        if run_timeout_seconds == 0 {
                            simple_call.await.map_err(|e| e.to_string())
                        } else {
                            match tokio::time::timeout(
                                Duration::from_secs(run_timeout_seconds),
                                simple_call,
                            )
                            .await
                            {
                                Ok(Ok(response)) => Ok(response),
                                Ok(Err(e)) => Err(e.to_string()),
                                Err(_) => Err(format!("Timed out after {run_timeout_seconds}s")),
                            }
                        }
                    } => res
                }
            };

            match result {
                Ok(response) => {
                    tracing::info!(
                        agent = %agent_label,
                        run_id = %run_id,
                        len = response.len(),
                        "Subagent completed"
                    );
                    registry
                        .complete(&run_id, SubagentOutcome::Success, Some(response))
                        .await;
                }
                Err(e) if e == "Cancelled" => {
                    tracing::info!(agent = %agent_label, run_id = %run_id, "Subagent cancelled");
                    registry
                        .complete(&run_id, SubagentOutcome::Cancelled, None)
                        .await;
                }
                Err(e) => {
                    tracing::error!(agent = %agent_label, run_id = %run_id, error = %e, "Subagent failed");
                    registry
                        .complete(&run_id, SubagentOutcome::Error(e.clone()), Some(e))
                        .await;
                }
            }
        });

        Ok(ToolResult {
            success: true,
            output: json!({
                "status": "accepted",
                "run_id": run_id,
                "agent": agent_name,
                "label": label,
                "model": model,
                "agentic": is_agentic,
                "run_timeout_seconds": run_timeout_seconds,
                "note": "Subagent spawned. Results will be automatically announced back to you when complete. No polling needed."
            })
            .to_string(),
            error: None,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> Arc<SubagentRegistry> {
        Arc::new(SubagentRegistry::new(5, 1))
    }

    fn test_tool() -> SubagentSpawnTool {
        SubagentSpawnTool::new(
            test_registry(),
            ProviderRuntimeOptions::default(),
            None,
            "invalid-test-provider".to_string(),
        )
    }

    #[test]
    fn name_and_schema() {
        let tool = test_tool();
        assert_eq!(tool.name(), "sessions_spawn");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["task"].is_object());
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["label"].is_object());
        assert!(schema["properties"]["model"].is_object());
        assert!(schema["properties"]["run_timeout_seconds"].is_object());
        assert!(schema["properties"]["timeout_seconds"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("task")));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn description_not_empty() {
        let tool = test_tool();
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn missing_task_param() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blank_task_rejected() {
        let tool = test_tool();
        let result = tool.execute(json!({"task": "  "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn unknown_agent_rejected() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"task": "do something", "agent": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown agent"));
    }

    #[tokio::test]
    async fn max_concurrent_enforced() {
        let registry = Arc::new(SubagentRegistry::new(1, 1));
        let record = SubagentRunRecord {
            run_id: "existing".to_string(),
            task: "task".to_string(),
            label: None,
            model: "model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: CancellationToken::new(),
            parent_context: None,
        };
        registry.register(record).await;

        let tool = SubagentSpawnTool::new(
            registry,
            ProviderRuntimeOptions::default(),
            None,
            "invalid-test-provider".to_string(),
        );
        let result = tool.execute(json!({"task": "another task"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Maximum concurrent"));
    }
}
