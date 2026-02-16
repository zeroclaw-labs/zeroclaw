use super::traits::{Tool, ToolResult};
use crate::config::Config;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Let the agent schedule tasks — cron jobs, one-shot delays, pause/resume.
pub struct ScheduleTool {
    config: Arc<Config>,
}

impl ScheduleTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "Manage scheduled tasks. Actions: 'add' (cron job), 'once' (one-shot delay like '30m'), 'list', 'remove', 'pause', 'resume'."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "once", "list", "remove", "pause", "resume"],
                    "description": "Action to perform"
                },
                "expression": {
                    "type": "string",
                    "description": "Cron expression (for 'add' action, e.g. '0 9 * * *')"
                },
                "delay": {
                    "type": "string",
                    "description": "Delay duration (for 'once' action, e.g. '30m', '2h', '1d')"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to execute (for 'add' and 'once' actions)"
                },
                "id": {
                    "type": "string",
                    "description": "Task ID (for 'remove', 'pause', 'resume' actions)"
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
            "list" => {
                let jobs = crate::cron::list_jobs(&self.config)?;
                if jobs.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No scheduled tasks.".into(),
                        error: None,
                    });
                }
                let mut lines = Vec::new();
                for job in &jobs {
                    let flags = match (job.paused, job.one_shot) {
                        (true, true) => " [paused, one-shot]",
                        (true, false) => " [paused]",
                        (false, true) => " [one-shot]",
                        (false, false) => "",
                    };
                    lines.push(format!(
                        "- {} | {} | next={} | cmd: {}{}",
                        job.id,
                        job.expression,
                        job.next_run.to_rfc3339(),
                        job.command,
                        flags,
                    ));
                }
                Ok(ToolResult {
                    success: true,
                    output: format!("{} task(s):\n{}", jobs.len(), lines.join("\n")),
                    error: None,
                })
            }
            "add" => {
                let expression = args
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'expression' for add"))?;
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'command' for add"))?;

                match crate::cron::add_job(&self.config, expression, command) {
                    Ok(job) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Added cron job {} | expr={} | next={} | cmd={}",
                            job.id,
                            job.expression,
                            job.next_run.to_rfc3339(),
                            job.command
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to add job: {e}")),
                    }),
                }
            }
            "once" => {
                let delay = args
                    .get("delay")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'delay' for once"))?;
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'command' for once"))?;

                match crate::cron::add_once(&self.config, delay, command) {
                    Ok(job) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Added one-shot task {} | runs_at={} | cmd={}",
                            job.id,
                            job.next_run.to_rfc3339(),
                            job.command
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to add one-shot task: {e}")),
                    }),
                }
            }
            "remove" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'id' for remove"))?;

                match crate::cron::remove_job(&self.config, id) {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Removed task {id}"),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to remove task: {e}")),
                    }),
                }
            }
            "pause" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'id' for pause"))?;

                match crate::cron::pause_job(&self.config, id) {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Paused task {id}"),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to pause task: {e}")),
                    }),
                }
            }
            "resume" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'id' for resume"))?;

                match crate::cron::resume_job(&self.config, id) {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!("Resumed task {id}"),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to resume task: {e}")),
                    }),
                }
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Use: add, once, list, remove, pause, resume"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Arc<Config> {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        Arc::new(config)
    }

    #[test]
    fn name_and_schema() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);
        assert_eq!(tool.name(), "schedule");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["command"].is_object());
    }

    #[tokio::test]
    async fn list_empty() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No scheduled tasks"));
    }

    #[tokio::test]
    async fn add_and_list() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);

        let result = tool
            .execute(json!({
                "action": "add",
                "expression": "0 0 9 * * *",
                "command": "echo hello"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Added cron job"));

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("1 task(s)"));
        assert!(result.output.contains("echo hello"));
    }

    #[tokio::test]
    async fn once_and_list() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);

        let result = tool
            .execute(json!({
                "action": "once",
                "delay": "30m",
                "command": "echo reminder"
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("one-shot"));

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.output.contains("[one-shot]"));
    }

    #[tokio::test]
    async fn pause_and_resume() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);

        let result = tool
            .execute(json!({
                "action": "add",
                "expression": "0 0 9 * * *",
                "command": "echo test"
            }))
            .await
            .unwrap();
        assert!(result.success);

        // Extract job ID from output
        let id = result
            .output
            .split(" | ")
            .next()
            .unwrap()
            .replace("Added cron job ", "");

        let result = tool
            .execute(json!({"action": "pause", "id": id}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Paused"));

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.output.contains("[paused]"));

        let result = tool
            .execute(json!({"action": "resume", "id": id}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Resumed"));
    }

    #[tokio::test]
    async fn remove_task() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);

        let result = tool
            .execute(json!({
                "action": "add",
                "expression": "0 0 9 * * *",
                "command": "echo bye"
            }))
            .await
            .unwrap();
        let id = result
            .output
            .split(" | ")
            .next()
            .unwrap()
            .replace("Added cron job ", "");

        let result = tool
            .execute(json!({"action": "remove", "id": id}))
            .await
            .unwrap();
        assert!(result.success);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.output.contains("No scheduled tasks"));
    }

    #[tokio::test]
    async fn max_tasks_enforced() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.scheduler.max_tasks = 1;
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let config = Arc::new(config);
        let tool = ScheduleTool::new(config);

        let result = tool
            .execute(json!({
                "action": "add",
                "expression": "0 0 9 * * *",
                "command": "echo first"
            }))
            .await
            .unwrap();
        assert!(result.success);

        let result = tool
            .execute(json!({
                "action": "add",
                "expression": "0 0 10 * * *",
                "command": "echo second"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Maximum"));
    }

    #[tokio::test]
    async fn unknown_action() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let tool = ScheduleTool::new(config);
        let result = tool
            .execute(json!({"action": "explode"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
