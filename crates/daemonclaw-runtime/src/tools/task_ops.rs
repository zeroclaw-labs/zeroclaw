use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

use crate::tasks::CURRENT_TASK_BINDING;
use daemonclaw_api::tool::{Tool, ToolResult};

pub struct TaskSubmitTool {
    workspace_dir: PathBuf,
    audit_config: daemonclaw_config::schema::AuditConfig,
}

impl TaskSubmitTool {
    pub fn new(workspace_dir: PathBuf, audit_config: daemonclaw_config::schema::AuditConfig) -> Self {
        Self {
            workspace_dir,
            audit_config,
        }
    }
}

#[async_trait]
impl Tool for TaskSubmitTool {
    fn name(&self) -> &str {
        "task_submit"
    }

    fn description(&self) -> &str {
        "Submit the current task for review. Call this when you believe the task \
         is complete and ready for acceptance verification. Only works during a \
         task-bound execution (heartbeat or --task mode)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let binding = match CURRENT_TASK_BINDING.try_with(|b| b.clone()) {
            Ok(Some(b)) => b,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: "No task binding — this tool is only available during task-bound execution.".to_string(),
                    error: None
                });
            }
        };

        let audit = match crate::security::audit::AuditLogger::new(
            self.audit_config.clone(),
            self.workspace_dir.clone(),
        ) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: format!("Failed to open audit log: {e}"),
                    error: None
                });
            }
        };

        let actor = crate::tasks::TaskActor {
            channel: "heartbeat".to_string(),
            id: Some(binding.actor_id.clone()),
        };

        match crate::tasks::store::submit_task(&self.workspace_dir, &binding.task_id, &actor, &audit)
        {
            Ok(task) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Task {} submitted for review (status: review).",
                    &task.id[..8.min(task.id.len())]
                ),
                error: None
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Failed to submit task: {e}"),
                error: None
            }),
        }
    }
}

pub struct TaskBlockTool {
    workspace_dir: PathBuf,
    audit_config: daemonclaw_config::schema::AuditConfig,
}

impl TaskBlockTool {
    pub fn new(workspace_dir: PathBuf, audit_config: daemonclaw_config::schema::AuditConfig) -> Self {
        Self {
            workspace_dir,
            audit_config,
        }
    }
}

#[async_trait]
impl Tool for TaskBlockTool {
    fn name(&self) -> &str {
        "task_block"
    }

    fn description(&self) -> &str {
        "Mark the current task as blocked. Call this when you encounter an obstacle \
         that prevents completing the task (missing dependency, permission issue, \
         unclear requirement, etc.). Only works during task-bound execution."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Why the task is blocked — describe the obstacle clearly so an operator can resolve it."
                }
            },
            "required": ["reason"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let binding = match CURRENT_TASK_BINDING.try_with(|b| b.clone()) {
            Ok(Some(b)) => b,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: "No task binding — this tool is only available during task-bound execution.".to_string(),
                    error: None
                });
            }
        };

        let reason = match args.get("reason").and_then(|v| v.as_str()) {
            Some(r) if !r.trim().is_empty() => r.trim().to_string(),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: "A non-empty 'reason' is required.".to_string(),
                    error: None
                });
            }
        };

        let audit = match crate::security::audit::AuditLogger::new(
            self.audit_config.clone(),
            self.workspace_dir.clone(),
        ) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: format!("Failed to open audit log: {e}"),
                    error: None
                });
            }
        };

        let actor = crate::tasks::TaskActor {
            channel: "heartbeat".to_string(),
            id: Some(binding.actor_id.clone()),
        };

        match crate::tasks::store::block_task(
            &self.workspace_dir,
            &binding.task_id,
            &actor,
            &reason,
            &audit,
        ) {
            Ok(task) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Task {} blocked: {reason}",
                    &task.id[..8.min(task.id.len())]
                ),
                error: None
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Failed to block task: {e}"),
                error: None
            }),
        }
    }
}
