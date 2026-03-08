use super::traits::{Tool, ToolResult};
use crate::agent::subagent_registry::{SubagentOutcome, SubagentRegistry};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool for managing spawned subagents: list, kill, or retrieve results.
pub struct SubagentsTool {
    registry: Arc<SubagentRegistry>,
}

impl SubagentsTool {
    pub fn new(registry: Arc<SubagentRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SubagentsTool {
    fn name(&self) -> &str {
        "subagents"
    }

    fn description(&self) -> &str {
        "Manage spawned subagents. Actions: 'list' (show all subagents), \
         'kill' (cancel a running subagent by run_id), \
         'result' (get the completed result for a run_id)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "kill", "result"],
                    "description": "The management action to perform"
                },
                "run_id": {
                    "type": "string",
                    "description": "The run_id of the subagent (required for 'kill' and 'result' actions)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "list" => self.handle_list().await,
            "kill" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'run_id' parameter for kill action"))?;
                self.handle_kill(run_id).await
            }
            "result" => {
                let run_id = args
                    .get("run_id")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("Missing 'run_id' parameter for result action")
                    })?;
                self.handle_result(run_id).await
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid actions: list, kill, result"
                )),
            }),
        }
    }
}

impl SubagentsTool {
    async fn handle_list(&self) -> anyhow::Result<ToolResult> {
        let records = self.registry.list_all().await;
        let entries: Vec<serde_json::Value> = records
            .iter()
            .map(|r| {
                let status = match &r.outcome {
                    None => "running".to_string(),
                    Some(SubagentOutcome::Success) => "completed".to_string(),
                    Some(SubagentOutcome::Error(e)) => format!("error: {e}"),
                    Some(SubagentOutcome::Cancelled) => "cancelled".to_string(),
                };
                let elapsed = r
                    .ended_at
                    .unwrap_or_else(std::time::Instant::now)
                    .duration_since(r.started_at);
                json!({
                    "run_id": r.run_id,
                    "task": r.task,
                    "label": r.label,
                    "model": r.model,
                    "status": status,
                    "elapsed_secs": elapsed.as_secs_f64(),
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&entries)
                .unwrap_or_else(|_| "[]".to_string()),
            error: None,
        })
    }

    async fn handle_kill(&self, run_id: &str) -> anyhow::Result<ToolResult> {
        if self.registry.get(run_id).await.is_none() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("No subagent found with run_id '{run_id}'")),
            });
        }

        if self.registry.cancel(run_id).await {
            Ok(ToolResult {
                success: true,
                output: format!("Subagent '{run_id}' cancellation requested"),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Subagent '{run_id}' is already completed and cannot be cancelled"
                )),
            })
        }
    }

    async fn handle_result(&self, run_id: &str) -> anyhow::Result<ToolResult> {
        let record = match self.registry.get(run_id).await {
            Some(r) => r,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("No subagent found with run_id '{run_id}'")),
                });
            }
        };

        if record.outcome.is_none() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Subagent '{run_id}' is still running. Use action 'list' to check status."
                )),
            });
        }

        let status = match &record.outcome {
            Some(SubagentOutcome::Success) => "completed",
            Some(SubagentOutcome::Error(_)) => "error",
            Some(SubagentOutcome::Cancelled) => "cancelled",
            None => unreachable!(),
        };

        let result = json!({
            "run_id": record.run_id,
            "task": record.task,
            "label": record.label,
            "model": record.model,
            "status": status,
            "result": record.result_text,
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result)
                .unwrap_or_else(|_| "{}".to_string()),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::subagent_registry::SubagentRunRecord;
    use tokio_util::sync::CancellationToken;

    fn test_registry() -> Arc<SubagentRegistry> {
        Arc::new(SubagentRegistry::new(5, 1))
    }

    async fn registry_with_record(
        run_id: &str,
        outcome: Option<SubagentOutcome>,
        result_text: Option<String>,
    ) -> Arc<SubagentRegistry> {
        let registry = test_registry();
        let mut record = SubagentRunRecord {
            run_id: run_id.to_string(),
            task: "test task".to_string(),
            label: Some("test-label".to_string()),
            model: "test-model".to_string(),
            started_at: std::time::Instant::now(),
            ended_at: None,
            outcome: None,
            result_text: None,
            cancellation_token: CancellationToken::new(),
        };
        if let Some(ref o) = outcome {
            record.ended_at = Some(std::time::Instant::now());
            record.outcome = Some(o.clone());
            record.result_text = result_text;
        }
        registry.register(record).await;
        registry
    }

    #[test]
    fn name_and_schema() {
        let tool = SubagentsTool::new(test_registry());
        assert_eq!(tool.name(), "subagents");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["run_id"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn description_not_empty() {
        let tool = SubagentsTool::new(test_registry());
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn missing_action_param() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_action() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool
            .execute(json!({"action": "unknown"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn list_empty() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("[]"));
    }

    #[tokio::test]
    async fn list_with_records() {
        let registry = registry_with_record("run-1", None, None).await;
        let tool = SubagentsTool::new(registry);
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("run-1"));
        assert!(result.output.contains("running"));
    }

    #[tokio::test]
    async fn kill_nonexistent() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool
            .execute(json!({"action": "kill", "run_id": "nope"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("No subagent found"));
    }

    #[tokio::test]
    async fn kill_active() {
        let registry = registry_with_record("run-2", None, None).await;
        let tool = SubagentsTool::new(registry);
        let result = tool
            .execute(json!({"action": "kill", "run_id": "run-2"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("cancellation requested"));
    }

    #[tokio::test]
    async fn kill_completed() {
        let registry =
            registry_with_record("run-3", Some(SubagentOutcome::Success), None).await;
        let tool = SubagentsTool::new(registry);
        let result = tool
            .execute(json!({"action": "kill", "run_id": "run-3"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("already completed"));
    }

    #[tokio::test]
    async fn kill_missing_run_id() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool.execute(json!({"action": "kill"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn result_nonexistent() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool
            .execute(json!({"action": "result", "run_id": "nope"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("No subagent found"));
    }

    #[tokio::test]
    async fn result_still_running() {
        let registry = registry_with_record("run-4", None, None).await;
        let tool = SubagentsTool::new(registry);
        let result = tool
            .execute(json!({"action": "result", "run_id": "run-4"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("still running"));
    }

    #[tokio::test]
    async fn result_completed() {
        let registry = registry_with_record(
            "run-5",
            Some(SubagentOutcome::Success),
            Some("the answer".to_string()),
        )
        .await;
        let tool = SubagentsTool::new(registry);
        let result = tool
            .execute(json!({"action": "result", "run_id": "run-5"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("the answer"));
        assert!(result.output.contains("completed"));
    }

    #[tokio::test]
    async fn result_missing_run_id() {
        let tool = SubagentsTool::new(test_registry());
        let result = tool.execute(json!({"action": "result"})).await;
        assert!(result.is_err());
    }
}
