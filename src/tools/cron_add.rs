use super::traits::{Tool, ToolResult, enforce_security_policy};
use crate::config::Config;
use crate::cron::{
    self, DeliveryConfig, JobType, Schedule, SessionTarget, delivery_json_schema,
    deserialize_maybe_stringified, format_schedule_error, schedule_json_schema,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct CronAddTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl CronAddTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }
}

#[async_trait]
impl Tool for CronAddTool {
    fn name(&self) -> &str {
        "cron_add"
    }

    fn description(&self) -> &str {
        "Create a scheduled cron job (shell or agent). \
         Requires job_type and schedule. \
         Shell example: schedule={\"kind\":\"cron\",\"expr\":\"0 9 * * 1-5\"}, job_type=\"shell\", command=\"echo hi\". \
         Agent example: schedule={\"kind\":\"every\",\"every_ms\":3600000}, job_type=\"agent\", prompt=\"check status\". \
         To deliver output to a channel, add delivery={\"mode\":\"announce\",\"channel\":\"discord\",\"to\":\"<id>\"}."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Optional human-readable name for the job"
                },
                "schedule": schedule_json_schema(),
                "job_type": {
                    "type": "string",
                    "enum": ["shell", "agent"],
                    "description": "Type of job: 'shell' runs a command (requires 'command'), 'agent' runs the AI agent (requires 'prompt')"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to run. Required when job_type='shell'."
                },
                "prompt": {
                    "type": "string",
                    "description": "Agent prompt to run on schedule. Required when job_type='agent'."
                },
                "session_target": {
                    "type": "string",
                    "enum": ["isolated", "main"],
                    "description": "Agent session context: 'isolated' starts a fresh session each run, 'main' reuses the primary session"
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for agent jobs, e.g. 'x-ai/grok-4-1-fast'"
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional allowlist of tool names for agent jobs. When omitted, all tools remain available."
                },
                "delivery": delivery_json_schema(),
                "delete_after_run": {
                    "type": "boolean",
                    "description": "If true, the job is automatically deleted after its first successful run. Defaults to true for 'at' schedules."
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk shell commands in supervised mode",
                    "default": false
                }
            },
            "required": ["schedule", "job_type"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.config.cron.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("cron is disabled by config (cron.enabled=false)".to_string()),
            });
        }

        let schedule = match args.get("schedule") {
            Some(v) => match deserialize_maybe_stringified::<Schedule>(v) {
                Ok(schedule) => schedule,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format_schedule_error(&e)),
                    });
                }
            },
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'schedule' parameter".to_string()),
                });
            }
        };

        let name = args
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);

        let job_type = match args.get("job_type").and_then(serde_json::Value::as_str) {
            Some("agent") => JobType::Agent,
            Some("shell") => JobType::Shell,
            Some(other) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Invalid job_type: {other}")),
                });
            }
            None => {
                if args.get("prompt").is_some() {
                    JobType::Agent
                } else {
                    JobType::Shell
                }
            }
        };

        let default_delete_after_run = matches!(schedule, Schedule::At { .. });
        let delete_after_run = args
            .get("delete_after_run")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default_delete_after_run);
        let approved = args
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let delivery = match args.get("delivery") {
            Some(v) => match serde_json::from_value::<DeliveryConfig>(v.clone()) {
                Ok(cfg) => Some(cfg),
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Invalid delivery config: {e}")),
                    });
                }
            },
            None => None,
        };

        let result = match job_type {
            JobType::Shell => {
                let command = match args.get("command").and_then(serde_json::Value::as_str) {
                    Some(command) if !command.trim().is_empty() => command,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing 'command' for shell job".to_string()),
                        });
                    }
                };

                if let Err(reason) = self.security.validate_command_execution(command, approved) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(reason),
                    });
                }

                if let Some(blocked) = enforce_security_policy(&self.security, "cron_add") {
                    return Ok(blocked);
                }

                cron::add_shell_job_with_approval(
                    &self.config,
                    name,
                    schedule,
                    command,
                    delivery,
                    approved,
                )
            }
            JobType::Agent => {
                let prompt = match args.get("prompt").and_then(serde_json::Value::as_str) {
                    Some(prompt) if !prompt.trim().is_empty() => prompt,
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing 'prompt' for agent job".to_string()),
                        });
                    }
                };

                let session_target = match args.get("session_target") {
                    Some(v) => match serde_json::from_value::<SessionTarget>(v.clone()) {
                        Ok(target) => target,
                        Err(e) => {
                            return Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(format!("Invalid session_target: {e}")),
                            });
                        }
                    },
                    None => SessionTarget::Isolated,
                };

                let model = args
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let allowed_tools = match args.get("allowed_tools") {
                    Some(v) => match serde_json::from_value::<Vec<String>>(v.clone()) {
                        Ok(v) => {
                            if v.is_empty() {
                                None // Treat empty list same as unset
                            } else {
                                Some(v)
                            }
                        }
                        Err(e) => {
                            return Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(format!("Invalid allowed_tools: {e}")),
                            });
                        }
                    },
                    None => None,
                };

                if let Some(blocked) = enforce_security_policy(&self.security, "cron_add") {
                    return Ok(blocked);
                }

                cron::add_agent_job(
                    &self.config,
                    name,
                    schedule,
                    prompt,
                    session_target,
                    model,
                    delivery,
                    delete_after_run,
                    allowed_tools,
                )
            }
        };

        match result {
            Ok(job) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "id": job.id,
                    "name": job.name,
                    "job_type": job.job_type,
                    "schedule": job.schedule,
                    "next_run": job.next_run,
                    "enabled": job.enabled,
                    "allowed_tools": job.allowed_tools
                }))?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::security::AutonomyLevel;
    use tempfile::TempDir;

    async fn test_config(tmp: &TempDir) -> Arc<Config> {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        Arc::new(config)
    }

    fn test_security(cfg: &Config) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::from_config(
            &cfg.autonomy,
            &cfg.workspace_dir,
        ))
    }

    #[tokio::test]
    async fn adds_shell_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));
        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "echo ok"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("next_run"));
    }

    #[tokio::test]
    async fn shell_job_persists_delivery() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));
        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "echo ok",
                "delivery": {
                    "mode": "announce",
                    "channel": "discord",
                    "to": "1234567890",
                    "best_effort": true
                }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);

        let jobs = cron::list_jobs(&cfg).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].delivery.mode, "announce");
        assert_eq!(jobs[0].delivery.channel.as_deref(), Some("discord"));
        assert_eq!(jobs[0].delivery.to.as_deref(), Some("1234567890"));
        assert!(jobs[0].delivery.best_effort);
    }

    #[tokio::test]
    async fn blocks_disallowed_shell_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.autonomy.allowed_commands = vec!["echo".into()];
        config.autonomy.level = AutonomyLevel::Supervised;
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "curl https://example.com"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("not allowed"));
    }

    #[tokio::test]
    async fn blocks_mutation_in_read_only_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.autonomy.level = AutonomyLevel::ReadOnly;
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "echo ok"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("read-only") || error.contains("not allowed"));
    }

    #[tokio::test]
    async fn blocks_add_when_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.autonomy.level = AutonomyLevel::Full;
        config.autonomy.max_actions_per_hour = 0;
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "echo ok"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Rate limit exceeded")
        );
        assert!(cron::list_jobs(&cfg).unwrap().is_empty());
    }

    #[tokio::test]
    async fn medium_risk_shell_command_requires_approval() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.autonomy.allowed_commands = vec!["touch".into()];
        config.autonomy.level = AutonomyLevel::Supervised;
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let denied = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "touch cron-approval-test"
            }))
            .await
            .unwrap();
        assert!(!denied.success);
        assert!(
            denied
                .error
                .unwrap_or_default()
                .contains("explicit approval")
        );

        let approved = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "touch cron-approval-test",
                "approved": true
            }))
            .await
            .unwrap();
        assert!(approved.success, "{:?}", approved.error);
    }

    #[tokio::test]
    async fn accepts_schedule_passed_as_json_string() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        // Simulate the LLM double-serializing the schedule: the value arrives
        // as a JSON string containing a JSON object, rather than an object.
        let result = tool
            .execute(json!({
                "schedule": r#"{"kind":"cron","expr":"*/5 * * * *"}"#,
                "job_type": "shell",
                "command": "echo string-schedule"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("next_run"));
    }

    #[tokio::test]
    async fn accepts_stringified_interval_schedule() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": r#"{"kind":"every","every_ms":60000}"#,
                "job_type": "shell",
                "command": "echo interval"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
    }

    #[tokio::test]
    async fn accepts_stringified_schedule_with_timezone() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": r#"{"kind":"cron","expr":"*/30 9-15 * * 1-5","tz":"Asia/Shanghai"}"#,
                "job_type": "shell",
                "command": "echo tz-test"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
    }

    #[tokio::test]
    async fn rejects_invalid_schedule() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "every", "every_ms": 0 },
                "job_type": "shell",
                "command": "echo nope"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("every_ms must be > 0")
        );
    }

    #[tokio::test]
    async fn agent_job_requires_prompt() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "agent"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Missing 'prompt'")
        );
    }

    #[tokio::test]
    async fn agent_job_persists_allowed_tools() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "agent",
                "prompt": "check status",
                "allowed_tools": ["file_read", "web_search"]
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);

        let jobs = cron::list_jobs(&cfg).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].allowed_tools,
            Some(vec!["file_read".into(), "web_search".into()])
        );
    }

    #[tokio::test]
    async fn empty_allowed_tools_stored_as_none() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "agent",
                "prompt": "check status",
                "allowed_tools": []
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);

        let jobs = cron::list_jobs(&cfg).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].allowed_tools, None,
            "empty allowed_tools should be stored as None"
        );
    }

    #[tokio::test]
    async fn delivery_schema_includes_matrix_channel() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg));

        let values =
            tool.parameters_schema()["properties"]["delivery"]["properties"]["channel"]["enum"]
                .as_array()
                .cloned()
                .unwrap_or_default();

        assert!(values.iter().any(|value| value == "matrix"));
    }

    #[test]
    fn schedule_schema_is_flat_object_with_kind_discriminator() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let security = Arc::new(SecurityPolicy::from_config(
            &cfg.autonomy,
            &cfg.workspace_dir,
        ));
        let tool = CronAddTool::new(cfg, security);
        let schema = tool.parameters_schema();

        // Top-level: schedule and job_type are required
        let top_required = schema["required"].as_array().expect("top-level required");
        assert!(top_required.iter().any(|v| v == "schedule"));
        assert!(top_required.iter().any(|v| v == "job_type"));

        // schedule is a flat object (no oneOf)
        let sched = &schema["properties"]["schedule"];
        assert!(
            sched.get("oneOf").is_none(),
            "schedule must NOT use oneOf (LLMs struggle with it)"
        );
        assert_eq!(sched["type"], "object");

        // kind is required and has the three enum values
        let sched_required = sched["required"].as_array().expect("schedule.required");
        assert!(sched_required.iter().any(|v| v == "kind"));

        let kind_enum = sched["properties"]["kind"]["enum"]
            .as_array()
            .expect("kind.enum");
        let kinds: Vec<&str> = kind_enum.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(kinds, vec!["cron", "at", "every"]);

        // All schedule fields are present
        let props = sched["properties"]
            .as_object()
            .expect("schedule.properties");
        assert!(props.contains_key("kind"));
        assert!(props.contains_key("expr"));
        assert!(props.contains_key("tz"));
        assert!(props.contains_key("at"));
        assert!(props.contains_key("every_ms"));
        assert_eq!(
            props["every_ms"]["type"].as_str(),
            Some("integer"),
            "every_ms must be typed as integer"
        );
    }
}
