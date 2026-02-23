//! Sub-agent listing tool.
//!
//! Implements the `subagent_list` tool for querying running and completed
//! sub-agent sessions with optional status filtering.

use super::subagent_registry::SubAgentRegistry;
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool that lists running and completed sub-agent sessions.
/// This is a read-only operation and does not require security enforcement
/// beyond the standard tool operation check.
pub struct SubAgentListTool {
    registry: Arc<SubAgentRegistry>,
}

impl SubAgentListTool {
    /// pub fn new.
    pub fn new(registry: Arc<SubAgentRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SubAgentListTool {
    fn name(&self) -> &str {
        "subagent_list"
    }

    fn description(&self) -> &str {
        "List running and completed background sub-agents. \
         Filter by status: running, completed, failed, killed, or all (default)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["running", "completed", "failed", "killed", "all"],
                    "description": "Filter by session status. Defaults to 'all'."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let status_filter = args.get("status").and_then(|v| v.as_str()).map(str::trim);

        // Validate the filter value
        if let Some(filter) = status_filter {
            if !filter.is_empty()
                && !matches!(
                    filter,
                    "running" | "completed" | "failed" | "killed" | "all"
                )
            {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid status filter '{filter}'. \
                         Must be one of: running, completed, failed, killed, all"
                    )),
                });
            }
        }

        let sessions = self.registry.list(status_filter);

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&sessions)?,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::subagent_registry::{SubAgentSession, SubAgentStatus};
    use chrono::Utc;

    fn make_registry() -> Arc<SubAgentRegistry> {
        Arc::new(SubAgentRegistry::new())
    }

    fn make_session(id: &str, agent: &str, task: &str) -> SubAgentSession {
        SubAgentSession {
            id: id.to_string(),
            agent_name: agent.to_string(),
            task: task.to_string(),
            status: SubAgentStatus::Running,
            started_at: Utc::now(),
            completed_at: None,
            result: None,
            handle: None,
        }
    }

    #[test]
    fn name_and_schema() {
        let tool = SubAgentListTool::new(make_registry());
        assert_eq!(tool.name(), "subagent_list");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["status"].is_object());
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn description_not_empty() {
        let tool = SubAgentListTool::new(make_registry());
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn list_empty_registry() {
        let tool = SubAgentListTool::new(make_registry());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output.trim(), "[]");
    }

    #[tokio::test]
    async fn list_all_sessions() {
        let registry = make_registry();
        registry.insert(make_session("s1", "researcher", "task1"));
        registry.insert(make_session("s2", "coder", "task2"));

        let tool = SubAgentListTool::new(registry);
        let result = tool.execute(json!({"status": "all"})).await.unwrap();
        assert!(result.success);

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[tokio::test]
    async fn list_filters_running() {
        let registry = make_registry();
        registry.insert(make_session("s1", "researcher", "task1"));
        registry.insert(make_session("s2", "coder", "task2"));
        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );

        let tool = SubAgentListTool::new(registry);
        let result = tool.execute(json!({"status": "running"})).await.unwrap();
        assert!(result.success);

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["session_id"], "s2");
    }

    #[tokio::test]
    async fn list_filters_completed() {
        let registry = make_registry();
        registry.insert(make_session("s1", "researcher", "task1"));
        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );

        let tool = SubAgentListTool::new(registry);
        let result = tool.execute(json!({"status": "completed"})).await.unwrap();
        assert!(result.success);

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["status"], "completed");
    }

    #[tokio::test]
    async fn list_filters_failed() {
        let registry = make_registry();
        registry.insert(make_session("s1", "agent", "task"));
        registry.fail("s1", "boom".to_string());

        let tool = SubAgentListTool::new(registry);
        let result = tool.execute(json!({"status": "failed"})).await.unwrap();
        assert!(result.success);

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["status"], "failed");
    }

    #[tokio::test]
    async fn list_default_shows_all() {
        let registry = make_registry();
        registry.insert(make_session("s1", "a", "t1"));
        registry.insert(make_session("s2", "b", "t2"));

        let tool = SubAgentListTool::new(registry);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[tokio::test]
    async fn invalid_status_filter() {
        let tool = SubAgentListTool::new(make_registry());
        let result = tool.execute(json!({"status": "invalid"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Invalid status filter"));
    }
}
