use super::shell::{BackgroundTaskRegistry, BackgroundTaskStatus};
use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Maximum lines to return when tailing a background task log.
const TAIL_MAX_LINES: usize = 100;
/// Maximum bytes to read from log file tail.
const TAIL_MAX_BYTES: usize = 65_536;

/// Companion tool for managing background shell tasks.
/// Supports: status, output (tail log), list, kill.
pub struct ShellStatusTool {
    registry: Arc<BackgroundTaskRegistry>,
}

impl ShellStatusTool {
    pub fn new(registry: Arc<BackgroundTaskRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for ShellStatusTool {
    fn name(&self) -> &str {
        "shell_status"
    }

    fn description(&self) -> &str {
        "Manage background shell tasks: check status, read output, list all, or kill a task. Important: wait at least 10 seconds between status checks to avoid wasting tool iterations. For long tasks, prefer reporting to the user and checking later rather than busy-waiting."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "output", "list", "kill"],
                    "description": "Action: status (check task state), output (tail log), list (all tasks), kill (terminate task)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (required for status, output, kill)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "list" => self.action_list(),
            "status" | "output" | "kill" => {
                let task_id = args
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("'task_id' is required for action '{action}'")
                    })?;
                match action {
                    "status" => self.action_status(task_id),
                    "output" => self.action_output(task_id).await,
                    "kill" => self.action_kill(task_id),
                    _ => unreachable!(),
                }
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: '{action}'. Use: status, output, list, kill"
                )),
                error_kind: None,
            }),
        }
    }
}

impl ShellStatusTool {
    fn action_list(&self) -> anyhow::Result<ToolResult> {
        let tasks = self.registry.list();
        if tasks.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No background tasks.".to_string(),
                error: None,
                error_kind: None,
            });
        }

        let entries: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                json!({
                    "task_id": t.id,
                    "command": t.command,
                    "pid": t.pid,
                    "status": status_string(&t.status),
                    "started_at": t.started_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string()),
            error: None,
            error_kind: None,
        })
    }

    fn action_status(&self, task_id: &str) -> anyhow::Result<ToolResult> {
        match self.registry.get(task_id) {
            Some(task) => Ok(ToolResult {
                success: true,
                output: json!({
                    "task_id": task.id,
                    "command": task.command,
                    "pid": task.pid,
                    "status": status_string(&task.status),
                    "started_at": task.started_at.to_rfc3339(),
                    "log_path": task.log_path.display().to_string(),
                })
                .to_string(),
                error: None,
                error_kind: None,
            }),
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("No task found with id '{task_id}'")),
                error_kind: None,
            }),
        }
    }

    async fn action_output(&self, task_id: &str) -> anyhow::Result<ToolResult> {
        let task = match self.registry.get(task_id) {
            Some(t) => t,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("No task found with id '{task_id}'")),
                    error_kind: None,
                });
            }
        };

        match tokio::fs::read(&task.log_path).await {
            Ok(bytes) => {
                // Take the tail of the log.
                let content = String::from_utf8_lossy(&bytes);
                let tail: String = if content.len() > TAIL_MAX_BYTES {
                    let start = content.len() - TAIL_MAX_BYTES;
                    let start = content.ceil_char_boundary(start);
                    format!("... [truncated]\n{}", &content[start..])
                } else {
                    content.to_string()
                };

                // Limit to last N lines.
                let lines: Vec<&str> = tail.lines().collect();
                let output = if lines.len() > TAIL_MAX_LINES {
                    let skip = lines.len() - TAIL_MAX_LINES;
                    format!("... [{skip} lines omitted]\n{}", lines[skip..].join("\n"))
                } else {
                    tail
                };

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                    error_kind: None,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Log file not found (task may not have started yet)".to_string()),
                error_kind: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to read log file: {e}")),
                error_kind: None,
            }),
        }
    }

    fn action_kill(&self, task_id: &str) -> anyhow::Result<ToolResult> {
        let task = match self.registry.get(task_id) {
            Some(t) => t,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("No task found with id '{task_id}'")),
                    error_kind: None,
                });
            }
        };

        if !matches!(task.status, BackgroundTaskStatus::Running) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Task '{task_id}' is not running (status: {})",
                    status_string(&task.status)
                )),
                error_kind: None,
            });
        }

        // Send SIGTERM via the kill command.
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("kill")
                .arg(task.pid.to_string())
                .status();
        }
        #[cfg(not(unix))]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &task.pid.to_string()])
                .status();
        }

        Ok(ToolResult {
            success: true,
            output: format!("Sent SIGTERM to task '{task_id}' (pid {}).", task.pid),
            error: None,
            error_kind: None,
        })
    }
}

fn status_string(status: &BackgroundTaskStatus) -> String {
    match status {
        BackgroundTaskStatus::Running => "running".to_string(),
        BackgroundTaskStatus::Completed { exit_code } => {
            format!("completed (exit code {exit_code})")
        }
        BackgroundTaskStatus::Failed { error } => format!("failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::shell::BackgroundTask;
    use std::path::PathBuf;

    fn test_registry_with_task() -> (Arc<BackgroundTaskRegistry>, String) {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        let task_id = "test-task-001".to_string();
        registry.insert(BackgroundTask {
            id: task_id.clone(),
            command: "echo test".to_string(),
            log_path: PathBuf::from("/dev/null"),
            pid: 12345,
            started_at: chrono::Utc::now(),
            status: BackgroundTaskStatus::Running,
        });
        (registry, task_id)
    }

    #[test]
    fn shell_status_tool_name() {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        let tool = ShellStatusTool::new(registry);
        assert_eq!(tool.name(), "shell_status");
    }

    #[tokio::test]
    async fn shell_status_list_empty() {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "list"}))
            .await
            .expect("list should succeed");
        assert!(result.success);
        assert!(result.output.contains("No background tasks"));
    }

    #[tokio::test]
    async fn shell_status_list_with_tasks() {
        let (registry, _) = test_registry_with_task();
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "list"}))
            .await
            .expect("list should succeed");
        assert!(result.success);
        assert!(result.output.contains("test-task-001"));
    }

    #[tokio::test]
    async fn shell_status_check_existing_task() {
        let (registry, task_id) = test_registry_with_task();
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "status", "task_id": task_id}))
            .await
            .expect("status should succeed");
        assert!(result.success);
        assert!(result.output.contains("running"));
    }

    #[tokio::test]
    async fn shell_status_check_missing_task() {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "status", "task_id": "nonexistent"}))
            .await
            .expect("status should return result for missing task");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("No task found"));
    }

    #[tokio::test]
    async fn shell_status_missing_action() {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        let tool = ShellStatusTool::new(registry);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_status_unknown_action() {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "dance"}))
            .await
            .expect("unknown action should return result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Unknown action"));
    }

    #[tokio::test]
    async fn shell_status_kill_not_running() {
        let (registry, task_id) = test_registry_with_task();
        registry.update_status(&task_id, BackgroundTaskStatus::Completed { exit_code: 0 });
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "kill", "task_id": task_id}))
            .await
            .expect("kill should return result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not running"));
    }

    #[tokio::test]
    async fn shell_status_output_missing_log() {
        let registry = Arc::new(BackgroundTaskRegistry::default());
        registry.insert(BackgroundTask {
            id: "log-test".to_string(),
            command: "echo".to_string(),
            log_path: PathBuf::from("/nonexistent/path/zeroclaw_test.log"),
            pid: 1,
            started_at: chrono::Utc::now(),
            status: BackgroundTaskStatus::Running,
        });
        let tool = ShellStatusTool::new(registry);
        let result = tool
            .execute(json!({"action": "output", "task_id": "log-test"}))
            .await
            .expect("output should return result");
        assert!(!result.success);
    }
}
