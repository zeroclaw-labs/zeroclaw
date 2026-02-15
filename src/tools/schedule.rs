use super::traits::{Tool, ToolResult};
use crate::config::Config;
use crate::cron;
use crate::security::SecurityPolicy;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::json;
use std::sync::Arc;

/// Tool that lets the AI agent manage scheduled tasks programmatically.
pub struct ScheduleTool {
    security: Arc<SecurityPolicy>,
    config: Config,
}

impl ScheduleTool {
    pub fn new(security: Arc<SecurityPolicy>, config: Config) -> Self {
        Self { security, config }
    }
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "Create, list, get, or cancel scheduled tasks (recurring cron or one-shot delays)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "get", "cancel"],
                    "description": "Action to perform"
                },
                "expression": {
                    "type": "string",
                    "description": "Cron expression for recurring jobs (e.g. '*/5 * * * *'). Use with action=create."
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to execute. Required for action=create."
                },
                "delay": {
                    "type": "string",
                    "description": "Human delay for one-shot jobs (e.g. '30s', '5m', '2h', '1d'). Use with action=create."
                },
                "run_at": {
                    "type": "string",
                    "description": "RFC 3339 timestamp for one-shot jobs (e.g. '2025-03-01T09:00:00Z'). Use with action=create."
                },
                "id": {
                    "type": "string",
                    "description": "Job ID. Required for action=get and action=cancel."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "list" => self.handle_list(),
            "get" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter for get action"))?;
                self.handle_get(id)
            }
            "create" => self.handle_create(&args),
            "cancel" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter for cancel action"))?;
                self.handle_cancel(id)
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: {action}. Use create, list, get, or cancel."
                )),
            }),
        }
    }
}

impl ScheduleTool {
    fn handle_list(&self) -> Result<ToolResult> {
        let jobs = cron::list_jobs(&self.config)?;
        if jobs.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No scheduled jobs.".into(),
                error: None,
            });
        }

        let mut lines = Vec::new();
        for job in &jobs {
            let kind = if job.one_shot {
                "one-shot"
            } else {
                "recurring"
            };
            let last = job
                .last_run
                .map_or_else(|| "never".into(), |d| d.to_rfc3339());
            let status = job.last_status.as_deref().unwrap_or("n/a");
            lines.push(format!(
                "- {} | {} ({}) | next={} | last={} ({}) | cmd: {}",
                job.id,
                job.expression,
                kind,
                job.next_run.to_rfc3339(),
                last,
                status,
                job.command,
            ));
        }

        Ok(ToolResult {
            success: true,
            output: format!("Scheduled jobs ({}):\n{}", jobs.len(), lines.join("\n")),
            error: None,
        })
    }

    fn handle_get(&self, id: &str) -> Result<ToolResult> {
        match cron::get_job(&self.config, id)? {
            Some(job) => {
                let detail = json!({
                    "id": job.id,
                    "expression": job.expression,
                    "command": job.command,
                    "next_run": job.next_run.to_rfc3339(),
                    "last_run": job.last_run.map(|d| d.to_rfc3339()),
                    "last_status": job.last_status,
                    "one_shot": job.one_shot,
                });
                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&detail)?,
                    error: None,
                })
            }
            None => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Job '{id}' not found")),
            }),
        }
    }

    fn handle_create(&self, args: &serde_json::Value) -> Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Security policy: read-only mode, cannot create jobs".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter for create action"))?;

        let has_expression = args.get("expression").and_then(|v| v.as_str()).is_some();
        let has_delay = args.get("delay").and_then(|v| v.as_str()).is_some();
        let has_run_at = args.get("run_at").and_then(|v| v.as_str()).is_some();

        let schedule_count = [has_expression, has_delay, has_run_at]
            .iter()
            .filter(|&&b| b)
            .count();

        if schedule_count != 1 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Exactly one of 'expression', 'delay', or 'run_at' must be provided".into(),
                ),
            });
        }

        if has_expression {
            let expression = args["expression"].as_str().unwrap();
            let job = cron::add_job(&self.config, expression, command)?;
            Ok(ToolResult {
                success: true,
                output: format!(
                    "Created recurring job {} (expr: {}, next: {}, cmd: {})",
                    job.id,
                    job.expression,
                    job.next_run.to_rfc3339(),
                    job.command
                ),
                error: None,
            })
        } else if has_delay {
            let delay_str = args["delay"].as_str().unwrap();
            let duration = parse_human_delay(delay_str)?;
            let run_at = Utc::now() + duration;
            let job = cron::add_one_shot_job(&self.config, run_at, command)?;
            Ok(ToolResult {
                success: true,
                output: format!(
                    "Created one-shot job {} (runs at: {}, cmd: {})",
                    job.id,
                    job.next_run.to_rfc3339(),
                    job.command
                ),
                error: None,
            })
        } else {
            let run_at_str = args["run_at"].as_str().unwrap();
            let run_at: DateTime<Utc> = DateTime::parse_from_rfc3339(run_at_str)
                .map_err(|e| anyhow::anyhow!("Invalid run_at timestamp: {e}"))?
                .with_timezone(&Utc);
            let job = cron::add_one_shot_job(&self.config, run_at, command)?;
            Ok(ToolResult {
                success: true,
                output: format!(
                    "Created one-shot job {} (runs at: {}, cmd: {})",
                    job.id,
                    job.next_run.to_rfc3339(),
                    job.command
                ),
                error: None,
            })
        }
    }

    fn handle_cancel(&self, id: &str) -> Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Security policy: read-only mode, cannot cancel jobs".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        match cron::remove_job(&self.config, id) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Cancelled job {id}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

/// Parse a human-readable delay string into a `chrono::Duration`.
///
/// Supported formats: `30s`, `5m`, `2h`, `1d`, `1w`.
/// Bare numbers (no suffix) default to minutes.
pub fn parse_human_delay(input: &str) -> Result<chrono::Duration> {
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!("Empty delay string");
    }

    let (num_str, unit) = if input.ends_with(|c: char| c.is_ascii_alphabetic()) {
        let split = input.len() - 1;
        (&input[..split], &input[split..])
    } else {
        (input, "m") // bare number → minutes
    };

    let n: u64 = num_str
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid number in delay: {input}"))?;

    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        "w" => n * 604_800,
        _ => anyhow::bail!("Unknown delay unit '{unit}' in '{input}'. Use s, m, h, d, or w."),
    };

    Ok(chrono::Duration::seconds(secs as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_setup() -> (TempDir, Config, Arc<SecurityPolicy>) {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        (tmp, config, security)
    }

    // ── parse_human_delay tests ──────────────────────────

    #[test]
    fn parse_seconds() {
        let d = parse_human_delay("30s").unwrap();
        assert_eq!(d.num_seconds(), 30);
    }

    #[test]
    fn parse_minutes() {
        let d = parse_human_delay("5m").unwrap();
        assert_eq!(d.num_seconds(), 300);
    }

    #[test]
    fn parse_hours() {
        let d = parse_human_delay("2h").unwrap();
        assert_eq!(d.num_seconds(), 7200);
    }

    #[test]
    fn parse_days() {
        let d = parse_human_delay("1d").unwrap();
        assert_eq!(d.num_seconds(), 86400);
    }

    #[test]
    fn parse_weeks() {
        let d = parse_human_delay("1w").unwrap();
        assert_eq!(d.num_seconds(), 604_800);
    }

    #[test]
    fn parse_bare_number_defaults_to_minutes() {
        let d = parse_human_delay("10").unwrap();
        assert_eq!(d.num_seconds(), 600);
    }

    #[test]
    fn parse_invalid_unit() {
        let err = parse_human_delay("10x").unwrap_err();
        assert!(err.to_string().contains("Unknown delay unit"));
    }

    #[test]
    fn parse_empty_string() {
        let err = parse_human_delay("").unwrap_err();
        assert!(err.to_string().contains("Empty"));
    }

    #[test]
    fn parse_invalid_number() {
        let err = parse_human_delay("abcm").unwrap_err();
        assert!(err.to_string().contains("Invalid number"));
    }

    // ── ScheduleTool metadata tests ──────────────────────

    #[test]
    fn tool_name_and_description() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);
        assert_eq!(tool.name(), "schedule");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn tool_schema_has_action() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
    }

    // ── list action ──────────────────────────────────────

    #[tokio::test]
    async fn list_empty() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No scheduled jobs"));
    }

    // ── create + list + get + cancel roundtrip ───────────

    #[tokio::test]
    async fn create_recurring_and_list() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({
                "action": "create",
                "expression": "*/5 * * * *",
                "command": "echo hello"
            }))
            .await
            .unwrap();
        assert!(result.success, "create failed: {:?}", result.error);
        assert!(result.output.contains("recurring"));

        let list_result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(list_result.success);
        assert!(list_result.output.contains("echo hello"));
        assert!(list_result.output.contains("recurring"));
    }

    #[tokio::test]
    async fn create_one_shot_with_delay() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({
                "action": "create",
                "delay": "30m",
                "command": "echo delayed"
            }))
            .await
            .unwrap();
        assert!(result.success, "create failed: {:?}", result.error);
        assert!(result.output.contains("one-shot"));
    }

    #[tokio::test]
    async fn create_one_shot_with_run_at() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({
                "action": "create",
                "run_at": "2030-01-01T00:00:00Z",
                "command": "echo future"
            }))
            .await
            .unwrap();
        assert!(result.success, "create failed: {:?}", result.error);
        assert!(result.output.contains("one-shot"));
        assert!(result.output.contains("2030"));
    }

    #[tokio::test]
    async fn create_rejects_multiple_schedule_params() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({
                "action": "create",
                "expression": "* * * * *",
                "delay": "30m",
                "command": "echo bad"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Exactly one"));
    }

    #[tokio::test]
    async fn create_rejects_no_schedule_param() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({
                "action": "create",
                "command": "echo missing"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Exactly one"));
    }

    #[tokio::test]
    async fn get_existing_job() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let create = tool
            .execute(json!({
                "action": "create",
                "expression": "*/10 * * * *",
                "command": "echo getme"
            }))
            .await
            .unwrap();
        assert!(create.success);

        // Extract ID from output: "Created recurring job <uuid> ..."
        let id = create.output.split_whitespace().nth(3).unwrap();

        let get = tool
            .execute(json!({"action": "get", "id": id}))
            .await
            .unwrap();
        assert!(get.success);
        assert!(get.output.contains("echo getme"));
    }

    #[tokio::test]
    async fn get_nonexistent_job() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({"action": "get", "id": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn cancel_existing_job() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let create = tool
            .execute(json!({
                "action": "create",
                "expression": "*/10 * * * *",
                "command": "echo cancel-me"
            }))
            .await
            .unwrap();
        assert!(create.success);

        let id = create.output.split_whitespace().nth(3).unwrap();

        let cancel = tool
            .execute(json!({"action": "cancel", "id": id}))
            .await
            .unwrap();
        assert!(cancel.success);
        assert!(cancel.output.contains("Cancelled"));

        // Verify it's gone
        let get = tool
            .execute(json!({"action": "get", "id": id}))
            .await
            .unwrap();
        assert!(!get.success);
    }

    #[tokio::test]
    async fn cancel_nonexistent_job() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool
            .execute(json!({"action": "cancel", "id": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let (_tmp, config, security) = test_setup();
        let tool = ScheduleTool::new(security, config);

        let result = tool.execute(json!({"action": "explode"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn readonly_blocks_create() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            autonomy: crate::config::AutonomyConfig {
                level: AutonomyLevel::ReadOnly,
                ..Default::default()
            },
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));

        let tool = ScheduleTool::new(security, config);
        let result = tool
            .execute(json!({
                "action": "create",
                "expression": "* * * * *",
                "command": "echo blocked"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn readonly_blocks_cancel() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            autonomy: crate::config::AutonomyConfig {
                level: AutonomyLevel::ReadOnly,
                ..Default::default()
            },
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));

        let tool = ScheduleTool::new(security, config);
        let result = tool
            .execute(json!({"action": "cancel", "id": "some-id"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn readonly_allows_list_and_get() {
        let tmp = TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            autonomy: crate::config::AutonomyConfig {
                level: AutonomyLevel::ReadOnly,
                ..Default::default()
            },
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));

        let tool = ScheduleTool::new(security, config);

        let list = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(list.success);

        let get = tool
            .execute(json!({"action": "get", "id": "nonexistent"}))
            .await
            .unwrap();
        // get should work (returns not-found, but doesn't block on security)
        assert!(!get.success);
        assert!(get.error.as_deref().unwrap().contains("not found"));
    }
}
