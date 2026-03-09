//! Sub-agent management tool (status and kill).
//!
//! Implements the `subagent_manage` tool for querying individual session
//! status and killing running sub-agents via cancellation tokens.

use super::subagent_registry::SubAgentRegistry;
use super::traits::{Tool, ToolResult};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool that manages running sub-agent sessions — check status or kill.
pub struct SubAgentManageTool {
    registry: Arc<SubAgentRegistry>,
    security: Arc<SecurityPolicy>,
}

impl SubAgentManageTool {
    /// pub fn new.
    pub fn new(registry: Arc<SubAgentRegistry>, security: Arc<SecurityPolicy>) -> Self {
        Self { registry, security }
    }
}

#[async_trait]
impl Tool for SubAgentManageTool {
    fn name(&self) -> &str {
        "subagent_manage"
    }

    fn description(&self) -> &str {
        "Manage a background sub-agent session. Actions: \
         'status' returns current status and partial output; \
         'kill' cancels a running session."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "session_id": {
                    "type": "string",
                    "minLength": 1,
                    "description": "The session ID returned by subagent_spawn"
                },
                "action": {
                    "type": "string",
                    "enum": ["kill", "status"],
                    "description": "Action to perform: 'kill' to cancel, 'status' to check"
                }
            },
            "required": ["session_id", "action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'session_id' parameter"))?;

        if session_id.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'session_id' parameter must not be empty".into()),
            });
        }

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        if action.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'action' parameter must not be empty".into()),
            });
        }

        match action {
            "status" => self.handle_status(session_id),
            "kill" => self.handle_kill(session_id),
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Must be one of: kill, status"
                )),
            }),
        }
    }
}

impl SubAgentManageTool {
    fn handle_status(&self, session_id: &str) -> anyhow::Result<ToolResult> {
        // Status is a read operation — no security enforcement needed
        match self.registry.get_status(session_id) {
            Some(snap) => {
                let duration_ms = snap.completed_at.map(|end| {
                    u64::try_from((end - snap.started_at).num_milliseconds()).unwrap_or_default()
                });

                let mut output = json!({
                    "session_id": session_id,
                    "agent": snap.agent_name,
                    "task": snap.task,
                    "status": snap.status.as_str(),
                    "started_at": snap.started_at.to_rfc3339(),
                    "duration_ms": duration_ms,
                });

                if let Some(end) = snap.completed_at {
                    output["completed_at"] = json!(end.to_rfc3339());
                }

                if let Some(ref r) = snap.result {
                    output["result"] = json!({
                        "success": r.success,
                        "output": if r.output.len() > 500 {
                            { let trunc_idx = r.output.char_indices().nth(500).map(|(i, _)| i).unwrap_or(r.output.len()); format!("{}... (truncated)", &r.output[..trunc_idx]) }
                        } else {
                            r.output.clone()
                        },
                        "error": r.error,
                    });
                }

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&output)?,
                    error: None,
                })
            }
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown session '{session_id}'")),
            }),
        }
    }

    fn handle_kill(&self, session_id: &str) -> anyhow::Result<ToolResult> {
        // Kill is a write operation — enforce security
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "subagent_manage:kill")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        if !self.registry.exists(session_id) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown session '{session_id}'")),
            });
        }

        if self.registry.kill(session_id) {
            Ok(ToolResult {
                success: true,
                output: json!({
                    "session_id": session_id,
                    "status": "killed",
                    "message": "Session cancelled successfully"
                })
                .to_string(),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Session '{session_id}' is not running (may have already completed or been killed)"
                )),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::subagent_registry::{SubAgentSession, SubAgentStatus};
    use chrono::Utc;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

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
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        assert_eq!(tool.name(), "subagent_manage");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["session_id"].is_object());
        assert!(schema["properties"]["action"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("session_id")));
        assert!(required.contains(&json!("action")));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn description_not_empty() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn missing_session_id() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool.execute(json!({"action": "status"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn missing_action() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool.execute(json!({"session_id": "s1"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blank_session_id_rejected() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool
            .execute(json!({"session_id": "  ", "action": "status"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn blank_action_rejected() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": " "}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn unknown_action_rejected() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": "restart"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn status_unknown_session() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool
            .execute(json!({"session_id": "nonexistent", "action": "status"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown session"));
    }

    #[tokio::test]
    async fn status_running_session() {
        let registry = make_registry();
        registry.insert(make_session("s1", "researcher", "find something"));

        let tool = SubAgentManageTool::new(registry, test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": "status"}))
            .await
            .unwrap();
        assert!(result.success);

        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "running");
        assert_eq!(output["agent"], "researcher");
        assert!(output["result"].is_null());
    }

    #[tokio::test]
    async fn status_completed_session() {
        let registry = make_registry();
        registry.insert(make_session("s1", "researcher", "find something"));
        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "found it".to_string(),
                error: None,
            },
        );

        let tool = SubAgentManageTool::new(registry, test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": "status"}))
            .await
            .unwrap();
        assert!(result.success);

        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "completed");
        assert_eq!(output["result"]["success"], true);
        assert!(output["completed_at"].is_string());
    }

    #[tokio::test]
    async fn status_truncates_long_output() {
        let registry = make_registry();
        registry.insert(make_session("s1", "agent", "task"));
        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "x".repeat(1000),
                error: None,
            },
        );

        let tool = SubAgentManageTool::new(registry, test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": "status"}))
            .await
            .unwrap();
        assert!(result.success);

        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let result_output = output["result"]["output"].as_str().unwrap();
        assert!(result_output.contains("truncated"));
        assert!(result_output.len() < 600);
    }

    #[tokio::test]
    async fn kill_running_session() {
        let registry = make_registry();
        registry.insert(make_session("s1", "researcher", "find something"));

        let tool = SubAgentManageTool::new(registry, test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": "kill"}))
            .await
            .unwrap();
        assert!(result.success);

        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["status"], "killed");
    }

    #[tokio::test]
    async fn kill_completed_session_fails() {
        let registry = make_registry();
        registry.insert(make_session("s1", "agent", "task"));
        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );

        let tool = SubAgentManageTool::new(registry, test_security());
        let result = tool
            .execute(json!({"session_id": "s1", "action": "kill"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not running"));
    }

    #[tokio::test]
    async fn kill_unknown_session() {
        let tool = SubAgentManageTool::new(make_registry(), test_security());
        let result = tool
            .execute(json!({"session_id": "nonexistent", "action": "kill"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown session"));
    }

    #[tokio::test]
    async fn kill_blocked_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let registry = make_registry();
        registry.insert(make_session("s1", "agent", "task"));

        let tool = SubAgentManageTool::new(registry, readonly);
        let result = tool
            .execute(json!({"session_id": "s1", "action": "kill"}))
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
    async fn kill_blocked_when_rate_limited() {
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let registry = make_registry();
        registry.insert(make_session("s1", "agent", "task"));

        let tool = SubAgentManageTool::new(registry, limited);
        let result = tool
            .execute(json!({"session_id": "s1", "action": "kill"}))
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
    async fn status_allowed_in_readonly_mode() {
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let registry = make_registry();
        registry.insert(make_session("s1", "agent", "task"));

        let tool = SubAgentManageTool::new(registry, readonly);
        let result = tool
            .execute(json!({"session_id": "s1", "action": "status"}))
            .await
            .unwrap();
        assert!(result.success);
    }
}
