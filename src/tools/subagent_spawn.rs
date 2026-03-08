use super::traits::{Tool, ToolResult};
use crate::agent::subagent_registry::{SubagentOutcome, SubagentRegistry, SubagentRunRecord};
use crate::providers::{self, Provider, ProviderRuntimeOptions};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Default model used when none is specified by the caller.
const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";

/// Tool that spawns a background subagent to execute a task asynchronously.
/// Returns immediately with a run ID that can be used to query status via
/// the `subagents` tool.
pub struct SubagentSpawnTool {
    registry: Arc<SubagentRegistry>,
    provider_runtime_options: ProviderRuntimeOptions,
    fallback_credential: Option<String>,
    default_provider: String,
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
        }
    }
}

#[async_trait]
impl Tool for SubagentSpawnTool {
    fn name(&self) -> &str {
        "subagent_spawn"
    }

    fn description(&self) -> &str {
        "Spawn a background subagent to work on a task asynchronously. Returns immediately with a \
         run_id. Use the 'subagents' tool to check status, get results, or cancel."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The task/prompt to send to the subagent"
                },
                "label": {
                    "type": "string",
                    "description": "Optional human-readable label for this subagent run"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model to use (e.g. 'openai/gpt-4o-mini'). Defaults to the configured model."
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

        let label = args
            .get("label")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        if !self.registry.can_spawn().await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Maximum concurrent subagents reached. Wait for one to finish or cancel an existing subagent.".into()),
            });
        }

        // Create provider for the subagent
        let provider_credential = self.fallback_credential.clone();
        let provider: Box<dyn Provider> = match providers::create_provider_with_options(
            &self.default_provider,
            provider_credential.as_deref(),
            &self.provider_runtime_options,
        ) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Failed to create provider '{}': {e}",
                        self.default_provider
                    )),
                });
            }
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let run_id_for_spawn = run_id.clone();
        let cancellation_token = CancellationToken::new();

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
        };

        self.registry.register(record).await;

        // Spawn background task
        let registry = self.registry.clone();
        let task_owned = task.to_string();
        let model_owned = model.clone();

        tokio::spawn(async move {
            let run_id = run_id_for_spawn;
            let result = tokio::select! {
                _ = cancellation_token.cancelled() => {
                    Err("Cancelled".to_string())
                }
                res = provider.chat_with_system(
                    None,
                    &task_owned,
                    &model_owned,
                    0.7,
                ) => {
                    res.map_err(|e| e.to_string())
                }
            };

            match result {
                Ok(response) => {
                    registry
                        .complete(&run_id, SubagentOutcome::Success, Some(response))
                        .await;
                }
                Err(e) if e == "Cancelled" => {
                    registry
                        .complete(&run_id, SubagentOutcome::Cancelled, None)
                        .await;
                }
                Err(e) => {
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
                "label": label,
                "model": model,
            })
            .to_string(),
            error: None,
        })
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
        assert_eq!(tool.name(), "subagent_spawn");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["task"].is_object());
        assert!(schema["properties"]["label"].is_object());
        assert!(schema["properties"]["model"].is_object());
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
        };
        registry.register(record).await;

        let tool = SubagentSpawnTool::new(
            registry,
            ProviderRuntimeOptions::default(),
            None,
            "invalid-test-provider".to_string(),
        );
        let result = tool
            .execute(json!({"task": "another task"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Maximum concurrent"));
    }
}
