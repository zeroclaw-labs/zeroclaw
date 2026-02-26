use super::traits::{Tool, ToolResult};
use crate::config::DelegateAgentConfig;
use crate::cosmic::{AgentPool, AgentRole, WorldModel};
use crate::providers::{self, Provider};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use futures::future::join_all;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const DEFAULT_MAX_PARALLEL: usize = 4;

#[derive(Debug, Clone)]
struct SubTask {
    description: String,
    model: Option<String>,
    system_prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Strategy {
    Parallel,
    Sequential,
    Consensus,
}

impl Strategy {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "parallel" => Some(Self::Parallel),
            "sequential" => Some(Self::Sequential),
            "consensus" => Some(Self::Consensus),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct SubTaskResult {
    index: usize,
    description: String,
    success: bool,
    output: String,
    error: Option<String>,
}

pub struct MultiDelegateTool {
    agents: Arc<HashMap<String, DelegateAgentConfig>>,
    security: Arc<SecurityPolicy>,
    fallback_credential: Option<String>,
    depth: u32,
    agent_pool: Option<Arc<Mutex<AgentPool>>>,
    world_beliefs: Option<Arc<Mutex<WorldModel>>>,
}

impl MultiDelegateTool {
    pub fn new(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            depth: 0,
            agent_pool: None,
            world_beliefs: None,
        }
    }

    pub fn with_depth(
        agents: HashMap<String, DelegateAgentConfig>,
        fallback_credential: Option<String>,
        security: Arc<SecurityPolicy>,
        depth: u32,
    ) -> Self {
        Self {
            agents: Arc::new(agents),
            security,
            fallback_credential,
            depth,
            agent_pool: None,
            world_beliefs: None,
        }
    }

    pub fn with_cosmic(
        mut self,
        pool: Arc<Mutex<AgentPool>>,
        world: Arc<Mutex<WorldModel>>,
    ) -> Self {
        self.agent_pool = Some(pool);
        self.world_beliefs = Some(world);
        self
    }

    fn pick_agent_config(&self, task: &SubTask) -> Option<(&String, &DelegateAgentConfig)> {
        if let Some(ref model_name) = task.model {
            self.agents
                .iter()
                .find(|(_, cfg)| cfg.model == *model_name)
                .or_else(|| self.agents.iter().next())
        } else {
            self.agents.iter().next()
        }
    }

    fn build_belief_prefix(&self) -> String {
        if let Some(ref world) = self.world_beliefs {
            let w = world.lock();
            let top = w.most_confident(5);
            if top.is_empty() {
                return String::new();
            }
            let entries: Vec<String> = top
                .iter()
                .map(|b| format!("{}: {:.2}", b.key, b.value))
                .collect();
            format!("[World context: {}]\n", entries.join(", "))
        } else {
            String::new()
        }
    }

    async fn run_single_task(
        &self,
        index: usize,
        task: &SubTask,
        extra_context: &str,
    ) -> SubTaskResult {
        let (agent_name, agent_config) = match self.pick_agent_config(task) {
            Some(pair) => pair,
            None => {
                return SubTaskResult {
                    index,
                    description: task.description.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("No agents configured".into()),
                };
            }
        };

        if self.depth >= agent_config.max_depth {
            return SubTaskResult {
                index,
                description: task.description.clone(),
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Delegation depth limit reached ({}/{})",
                    self.depth, agent_config.max_depth
                )),
            };
        }

        if let Some(ref pool) = self.agent_pool {
            let mut p = pool.lock();
            let label = format!("{agent_name}_{index}");
            p.register_agent(&label, AgentRole::Advisor);
        }

        let provider_credential_owned = agent_config
            .api_key
            .clone()
            .or_else(|| self.fallback_credential.clone());
        #[allow(clippy::option_as_ref_deref)]
        let provider_credential = provider_credential_owned.as_ref().map(String::as_str);

        let provider: Box<dyn Provider> =
            match providers::create_provider(&agent_config.provider, provider_credential) {
                Ok(p) => p,
                Err(e) => {
                    self.cleanup_pool(agent_name, index);
                    return SubTaskResult {
                        index,
                        description: task.description.clone(),
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Failed to create provider '{}': {e}",
                            agent_config.provider
                        )),
                    };
                }
            };

        let belief_prefix = self.build_belief_prefix();
        let system = task
            .system_prompt
            .as_deref()
            .or(agent_config.system_prompt.as_deref());
        let full_prompt = if extra_context.is_empty() {
            format!("{belief_prefix}{}", task.description)
        } else {
            format!(
                "{belief_prefix}[Prior context]\n{extra_context}\n\n[Task]\n{}",
                task.description
            )
        };

        let temperature = agent_config.temperature.unwrap_or(0.7);
        let timeout_secs = DEFAULT_TIMEOUT_SECS;

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            provider.chat_with_system(system, &full_prompt, &agent_config.model, temperature),
        )
        .await;

        self.cleanup_pool(agent_name, index);

        match result {
            Ok(Ok(response)) => {
                let output = if response.trim().is_empty() {
                    "[Empty response]".to_string()
                } else {
                    response
                };
                SubTaskResult {
                    index,
                    description: task.description.clone(),
                    success: true,
                    output: format!(
                        "[Agent '{agent_name}' ({}/{})]\n{output}",
                        agent_config.provider, agent_config.model
                    ),
                    error: None,
                }
            }
            Ok(Err(e)) => SubTaskResult {
                index,
                description: task.description.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Agent '{agent_name}' failed: {e}")),
            },
            Err(_) => SubTaskResult {
                index,
                description: task.description.clone(),
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Agent '{agent_name}' timed out after {timeout_secs}s"
                )),
            },
        }
    }

    fn cleanup_pool(&self, agent_name: &str, index: usize) {
        if let Some(ref pool) = self.agent_pool {
            let mut p = pool.lock();
            let label = format!("{agent_name}_{index}");
            p.remove_agent(&label);
        }
    }

    async fn execute_parallel(&self, tasks: &[SubTask]) -> Vec<SubTaskResult> {
        let mut handles = Vec::with_capacity(tasks.len());
        for chunk in tasks.chunks(DEFAULT_MAX_PARALLEL) {
            let mut chunk_futures = Vec::with_capacity(chunk.len());
            for (chunk_idx, task) in chunk.iter().enumerate() {
                let global_idx = handles.len() + chunk_idx;
                chunk_futures.push(self.run_single_task(global_idx, task, ""));
            }
            let chunk_results = join_all(chunk_futures).await;
            handles.extend(chunk_results);
        }
        handles
    }

    async fn execute_sequential(&self, tasks: &[SubTask]) -> Vec<SubTaskResult> {
        let mut results = Vec::with_capacity(tasks.len());
        let mut accumulated_context = String::new();

        for (i, task) in tasks.iter().enumerate() {
            let result = self.run_single_task(i, task, &accumulated_context).await;
            if result.success {
                if !accumulated_context.is_empty() {
                    accumulated_context.push_str("\n\n---\n\n");
                }
                accumulated_context.push_str(&format!(
                    "Task {}: {}\nResult: {}",
                    i + 1,
                    task.description,
                    result.output
                ));
            }
            results.push(result);
        }
        results
    }

    async fn execute_consensus(&self, tasks: &[SubTask]) -> Vec<SubTaskResult> {
        let mut results = self.execute_parallel(tasks).await;

        let success_count = results.iter().filter(|r| r.success).count();

        if success_count < 2 {
            return results;
        }

        let successful_outputs: Vec<(usize, String, String)> = results
            .iter()
            .filter(|r| r.success)
            .map(|r| {
                let body = r
                    .output
                    .lines()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_lowercase();
                (r.index, r.description.clone(), body)
            })
            .collect();

        let all_agree = successful_outputs
            .windows(2)
            .all(|pair| pair[0].2 == pair[1].2);

        if all_agree {
            results.push(SubTaskResult {
                index: results.len(),
                description: "[Consensus]".into(),
                success: true,
                output: format!("All {success_count} agents agree on the result."),
                error: None,
            });
        } else {
            let disagreements: Vec<String> = successful_outputs
                .iter()
                .map(|(idx, desc, body)| {
                    let truncated: String = body.chars().take(200).collect();
                    format!("Task {} ({desc}): {truncated}", idx + 1)
                })
                .collect();
            results.push(SubTaskResult {
                index: results.len(),
                description: "[Consensus]".into(),
                success: true,
                output: format!(
                    "Agents DISAGREE ({success_count} responses). Summaries:\n{}",
                    disagreements.join("\n")
                ),
                error: None,
            });
        }

        results
    }

    fn format_results(results: &[SubTaskResult]) -> String {
        let succeeded = results.iter().filter(|r| r.success).count();
        let failed = results.iter().filter(|r| !r.success).count();

        let mut output = format!(
            "Multi-delegate complete: {succeeded} succeeded, {failed} failed, {} total\n\n",
            results.len()
        );

        for r in results {
            if r.success {
                output.push_str(&format!(
                    "--- Task {} [OK] ---\n{}\n\n",
                    r.index + 1,
                    r.output
                ));
            } else {
                output.push_str(&format!(
                    "--- Task {} [FAILED] ---\n{}\n\n",
                    r.index + 1,
                    r.error.as_deref().unwrap_or("Unknown error")
                ));
            }
        }

        output
    }
}

#[async_trait]
impl Tool for MultiDelegateTool {
    fn name(&self) -> &str {
        "multi_delegate"
    }

    fn description(&self) -> &str {
        "Delegate multiple subtasks to agents with parallel execution and result aggregation. \
         Supports parallel (all at once), sequential (chained context), and consensus \
         (compare results) strategies."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let agent_names: Vec<&str> = self.agents.keys().map(|s| s.as_str()).collect();
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "description": {
                                "type": "string",
                                "minLength": 1,
                                "description": "The task/prompt to send to the sub-agent"
                            },
                            "model": {
                                "type": "string",
                                "description": format!(
                                    "Optional model override. Available agents: {}",
                                    if agent_names.is_empty() {
                                        "(none configured)".to_string()
                                    } else {
                                        agent_names.join(", ")
                                    }
                                )
                            },
                            "system_prompt": {
                                "type": "string",
                                "description": "Optional system prompt override for this sub-task"
                            }
                        },
                        "required": ["description"]
                    },
                    "description": "Array of sub-tasks to delegate"
                },
                "strategy": {
                    "type": "string",
                    "enum": ["parallel", "sequential", "consensus"],
                    "default": "parallel",
                    "description": "Execution strategy: parallel (all at once), sequential (chained context), consensus (compare results)"
                }
            },
            "required": ["tasks"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "multi_delegate")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let tasks_val = args
            .get("tasks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'tasks' parameter"))?;

        if tasks_val.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'tasks' array must not be empty".into()),
            });
        }

        let mut tasks = Vec::with_capacity(tasks_val.len());
        for (i, t) in tasks_val.iter().enumerate() {
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            if description.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Task at index {i} has empty description")),
                });
            }
            tasks.push(SubTask {
                description: description.to_string(),
                model: t
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string()),
                system_prompt: t
                    .get("system_prompt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string()),
            });
        }

        let strategy_str = args
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("parallel");

        let strategy = match Strategy::from_str(strategy_str) {
            Some(s) => s,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown strategy '{strategy_str}'. Must be: parallel, sequential, consensus"
                    )),
                });
            }
        };

        let results = match strategy {
            Strategy::Parallel => self.execute_parallel(&tasks).await,
            Strategy::Sequential => self.execute_sequential(&tasks).await,
            Strategy::Consensus => self.execute_consensus(&tasks).await,
        };

        let any_success = results.iter().any(|r| r.success);
        let output = Self::format_results(&results);

        Ok(ToolResult {
            success: any_success,
            output,
            error: if any_success {
                None
            } else {
                Some("All sub-tasks failed".into())
            },
        })
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
            },
        );
        agents.insert(
            "coder".to_string(),
            DelegateAgentConfig {
                provider: "openrouter".to_string(),
                model: "anthropic/claude-sonnet-4-20250514".to_string(),
                system_prompt: None,
                api_key: Some("test-credential".to_string()),
                temperature: None,
                max_depth: 2,
            },
        );
        agents
    }

    #[test]
    fn name_and_description() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        assert_eq!(tool.name(), "multi_delegate");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_is_valid() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["tasks"].is_object());
        assert!(schema["properties"]["strategy"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("tasks")));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["properties"]["strategy"]["enum"],
            json!(["parallel", "sequential", "consensus"])
        );
        assert_eq!(schema["properties"]["tasks"]["minItems"], json!(1));
    }

    #[test]
    fn schema_lists_agents() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["tasks"]["items"]["properties"]["model"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("researcher") || desc.contains("coder"));
    }

    #[test]
    fn empty_agents_schema() {
        let tool = MultiDelegateTool::new(HashMap::new(), None, test_security());
        let schema = tool.parameters_schema();
        let desc = schema["properties"]["tasks"]["items"]["properties"]["model"]["description"]
            .as_str()
            .unwrap();
        assert!(desc.contains("none configured"));
    }

    #[tokio::test]
    async fn missing_tasks_param() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_tasks_array() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let result = tool.execute(json!({"tasks": []})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn task_with_empty_description() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({"tasks": [{"description": "  "}]}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("empty description"));
    }

    #[tokio::test]
    async fn unknown_strategy() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let result = tool
            .execute(json!({
                "tasks": [{"description": "test"}],
                "strategy": "unknown"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown strategy"));
    }

    #[tokio::test]
    async fn blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MultiDelegateTool::new(sample_agents(), None, readonly);
        let result = tool
            .execute(json!({"tasks": [{"description": "test"}]}))
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
    async fn blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = MultiDelegateTool::new(sample_agents(), None, limited);
        let result = tool
            .execute(json!({"tasks": [{"description": "test"}]}))
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
    async fn depth_limit_enforced() {
        let tool = MultiDelegateTool::with_depth(sample_agents(), None, test_security(), 3);
        let result = tool
            .execute(json!({
                "tasks": [{"description": "test task"}]
            }))
            .await
            .unwrap();
        assert!(result.output.contains("depth limit"));
    }

    #[tokio::test]
    async fn parallel_with_invalid_provider() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            DelegateAgentConfig {
                provider: "invalid-provider".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 5,
            },
        );
        let tool = MultiDelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "tasks": [
                    {"description": "task one"},
                    {"description": "task two"}
                ],
                "strategy": "parallel"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("FAILED"));
    }

    #[tokio::test]
    async fn sequential_with_invalid_provider() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            DelegateAgentConfig {
                provider: "invalid-provider".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 5,
            },
        );
        let tool = MultiDelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "tasks": [
                    {"description": "task one"},
                    {"description": "task two"}
                ],
                "strategy": "sequential"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("FAILED"));
    }

    #[tokio::test]
    async fn consensus_with_invalid_provider() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            DelegateAgentConfig {
                provider: "invalid-provider".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 5,
            },
        );
        let tool = MultiDelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "tasks": [
                    {"description": "task one"},
                    {"description": "task two"}
                ],
                "strategy": "consensus"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("FAILED"));
    }

    #[tokio::test]
    async fn no_agents_configured() {
        let tool = MultiDelegateTool::new(HashMap::new(), None, test_security());
        let result = tool
            .execute(json!({
                "tasks": [{"description": "test"}]
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.output.contains("No agents configured"));
    }

    #[tokio::test]
    async fn default_strategy_is_parallel() {
        let mut agents = HashMap::new();
        agents.insert(
            "broken".to_string(),
            DelegateAgentConfig {
                provider: "invalid-provider".to_string(),
                model: "model".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 5,
            },
        );
        let tool = MultiDelegateTool::new(agents, None, test_security());
        let result = tool
            .execute(json!({
                "tasks": [{"description": "task one"}, {"description": "task two"}]
            }))
            .await
            .unwrap();
        assert!(result.output.contains("2 total"));
    }

    #[test]
    fn strategy_from_str_valid() {
        assert_eq!(Strategy::from_str("parallel"), Some(Strategy::Parallel));
        assert_eq!(Strategy::from_str("sequential"), Some(Strategy::Sequential));
        assert_eq!(Strategy::from_str("consensus"), Some(Strategy::Consensus));
        assert_eq!(Strategy::from_str("invalid"), None);
    }

    #[test]
    fn format_results_output() {
        let results = vec![
            SubTaskResult {
                index: 0,
                description: "task one".into(),
                success: true,
                output: "result one".into(),
                error: None,
            },
            SubTaskResult {
                index: 1,
                description: "task two".into(),
                success: false,
                output: String::new(),
                error: Some("provider error".into()),
            },
        ];
        let output = MultiDelegateTool::format_results(&results);
        assert!(output.contains("1 succeeded"));
        assert!(output.contains("1 failed"));
        assert!(output.contains("2 total"));
        assert!(output.contains("result one"));
        assert!(output.contains("provider error"));
    }

    #[test]
    fn depth_construction() {
        let tool = MultiDelegateTool::with_depth(sample_agents(), None, test_security(), 5);
        assert_eq!(tool.depth, 5);
    }

    #[test]
    fn tool_spec_generation() {
        let tool = MultiDelegateTool::new(sample_agents(), None, test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "multi_delegate");
        assert!(!spec.description.is_empty());
        assert!(spec.parameters.is_object());
    }
}
