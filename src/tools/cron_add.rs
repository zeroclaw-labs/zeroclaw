use super::traits::{Tool, ToolResult};
use crate::config::Config;
use crate::cron::{
    self, deserialize_maybe_stringified, DeliveryConfig, JobType, Schedule, SessionTarget,
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

    fn enforce_mutation_allowed(&self, action: &str) -> Option<ToolResult> {
        if !self.security.can_act() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Security policy: read-only mode, cannot perform '{action}'"
                )),
            });
        }

        if self.security.is_rate_limited() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".to_string()),
            });
        }

        if !self.security.record_action() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".to_string()),
            });
        }

        None
    }
}

#[async_trait]
impl Tool for CronAddTool {
    fn name(&self) -> &str {
        "cron_add"
    }

    fn description(&self) -> &str {
        "Create a scheduled cron job (shell or agent) with cron/at/every schedules. \
         Use job_type='agent' with a prompt to run the AI agent on schedule. \
         To deliver output to a channel (Discord, Telegram, Slack, Mattermost, Matrix, QQ), set \
         delivery={\"mode\":\"announce\",\"channel\":\"discord\",\"to\":\"<channel_id_or_chat_id>\"}. \
         This is the preferred tool for sending scheduled/delayed messages to users via channels."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Optional human-readable name for the job"
                },
                // NOTE: oneOf is correct for OpenAI-compatible APIs (including OpenRouter).
                // Gemini does not support oneOf in tool schemas; if Gemini native tool calling
                // is ever wired up, SchemaCleanr::clean_for_gemini must be applied before
                // tool specs are sent. See src/tools/schema.rs.
                "schedule": {
                    "description": "When to run the job. Exactly one of three forms must be used.",
                    "oneOf": [
                        {
                            "type": "object",
                            "description": "Cron expression schedule (repeating). Example: {\"kind\":\"cron\",\"expr\":\"0 9 * * 1-5\",\"tz\":\"America/New_York\"}",
                            "properties": {
                                "kind": { "type": "string", "enum": ["cron"] },
                                "expr": { "type": "string", "description": "Standard 5-field cron expression, e.g. '*/5 * * * *'" },
                                "tz": { "type": "string", "description": "Optional IANA timezone name, e.g. 'America/New_York'. Defaults to UTC." }
                            },
                            "required": ["kind", "expr"]
                        },
                        {
                            "type": "object",
                            "description": "One-shot schedule at a specific UTC datetime. Example: {\"kind\":\"at\",\"at\":\"2025-12-31T23:59:00Z\"}",
                            "properties": {
                                "kind": { "type": "string", "enum": ["at"] },
                                "at": { "type": "string", "description": "ISO 8601 UTC datetime string, e.g. '2025-12-31T23:59:00Z'" }
                            },
                            "required": ["kind", "at"]
                        },
                        {
                            "type": "object",
                            "description": "Repeating interval schedule in milliseconds. Example: {\"kind\":\"every\",\"every_ms\":3600000} runs every hour.",
                            "properties": {
                                "kind": { "type": "string", "enum": ["every"] },
                                "every_ms": { "type": "integer", "description": "Interval in milliseconds, e.g. 3600000 for every hour" }
                            },
                            "required": ["kind", "every_ms"]
                        }
                    ]
                },
                "job_type": {
                    "type": "string",
                    "enum": ["shell", "agent"],
                    "description": "Type of job: 'shell' runs a command, 'agent' runs the AI agent with a prompt"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to run (required when job_type is 'shell')"
                },
                "prompt": {
                    "type": "string",
                    "description": "Agent prompt to run on schedule (required when job_type is 'agent')"
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
                "delivery": {
                    "type": "object",
                    "description": "Optional delivery config to send job output to a channel after each run. When provided, all three of mode, channel, and to are expected.",
                    "properties": {
                        "mode": {
                            "type": "string",
                            "enum": ["none", "announce"],
                            "description": "'announce' sends output to the specified channel; 'none' disables delivery"
                        },
                        "channel": {
                            "type": "string",
                            "enum": ["telegram", "discord", "slack", "mattermost", "matrix", "qq"],
                            "description": "Channel type to deliver output to"
                        },
                        "to": {
                            "type": "string",
                            "description": "Destination ID: Discord channel ID, Telegram chat ID, Slack channel name, etc."
                        },
                        "best_effort": {
                            "type": "boolean",
                            "description": "If true, a delivery failure does not fail the job itself. Defaults to true."
                        }
                    }
                },
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
            "required": ["schedule"]
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
                        error: Some(format!("Invalid schedule: {e}")),
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

                if let Some(blocked) = self.enforce_mutation_allowed("cron_add") {
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

                if let Some(blocked) = self.enforce_mutation_allowed("cron_add") {
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
        assert!(result
            .error
            .unwrap_or_default()
            .contains("Rate limit exceeded"));
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
        assert!(denied
            .error
            .unwrap_or_default()
            .contains("explicit approval"));

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
        assert!(result
            .error
            .unwrap_or_default()
            .contains("every_ms must be > 0"));
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
        assert!(result
            .error
            .unwrap_or_default()
            .contains("Missing 'prompt'"));
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

        let values = tool.parameters_schema()["properties"]["delivery"]["properties"]["channel"]
            ["enum"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        assert!(values.iter().any(|value| value == "matrix"));
    }

    #[test]
    fn schedule_schema_is_oneof_with_cron_at_every_variants() {
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

        // Top-level: schedule is required
        let top_required = schema["required"].as_array().expect("top-level required");
        assert!(top_required.iter().any(|v| v == "schedule"));

        // schedule is a oneOf with exactly 3 variants: cron, at, every
        let one_of = schema["properties"]["schedule"]["oneOf"]
            .as_array()
            .expect("schedule.oneOf must be an array");
        assert_eq!(one_of.len(), 3, "expected cron, at, and every variants");

        let kinds: Vec<&str> = one_of
            .iter()
            .filter_map(|v| v["properties"]["kind"]["enum"][0].as_str())
            .collect();
        assert!(kinds.contains(&"cron"), "missing cron variant");
        assert!(kinds.contains(&"at"), "missing at variant");
        assert!(kinds.contains(&"every"), "missing every variant");

        // Each variant declares its required fields and every_ms is typed integer
        for variant in one_of {
            let kind = variant["properties"]["kind"]["enum"][0]
                .as_str()
                .expect("variant kind");
            let req: Vec<&str> = variant["required"]
                .as_array()
                .unwrap_or_else(|| panic!("{kind} variant must have required"))
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            assert!(
                req.contains(&"kind"),
                "{kind} variant missing 'kind' in required"
            );
            match kind {
                "cron" => assert!(req.contains(&"expr"), "cron variant missing 'expr'"),
                "at" => assert!(req.contains(&"at"), "at variant missing 'at'"),
                "every" => {
                    assert!(
                        req.contains(&"every_ms"),
                        "every variant missing 'every_ms'"
                    );
                    assert_eq!(
                        variant["properties"]["every_ms"]["type"].as_str(),
                        Some("integer"),
                        "every_ms must be typed as integer"
                    );
                }
                _ => panic!("unexpected kind: {kind}"),
            }
        }
    }
}
