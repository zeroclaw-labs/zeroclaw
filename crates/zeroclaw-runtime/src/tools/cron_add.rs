use super::cron_common::{
    AT_DESCRIPTION, CRON_TZ_DESCRIPTION, cron_add_output, deserialize_schedule_arg,
};
use crate::cron::{self, DeliveryConfig, JobType, Schedule, SessionTarget};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{Value, json};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronAddTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
    /// Owning agent — the alias of the agent whose tool loop registered
    /// this tool instance. Cron jobs created here are validated against
    /// this agent's risk profile and run as this agent.
    agent_alias: String,
}

impl CronAddTool {
    pub fn new(
        config: Arc<Config>,
        security: Arc<SecurityPolicy>,
        agent_alias: impl Into<String>,
    ) -> Self {
        Self {
            config,
            security,
            agent_alias: agent_alias.into(),
        }
    }

    fn plain_string_schedule_error(raw: &str) -> Option<String> {
        let schedule = raw.trim();
        if schedule.starts_with('{') {
            return None;
        }

        let got = serde_json::to_string(schedule).unwrap_or_else(|_| "\"<invalid>\"".to_string());
        Some(format!(
            "Invalid schedule: expected a JSON object with a \"kind\" field, got plain string {got}. \
             Use one of: {{\"kind\":\"cron\",\"expr\":\"0 9 * * 1-5\"}}, \
             {{\"kind\":\"at\",\"at\":\"2025-12-31T23:59:00Z\"}}, \
             {{\"kind\":\"after\",\"after_seconds\":600}} for one-shot relative reminders, or \
             {{\"kind\":\"every\",\"every_ms\":3600000}}"
        ))
    }

    fn deserialize_cron_add_schedule_arg(value: &Value) -> Result<CronAddScheduleArg, String> {
        if let Some(normalized) = normalize_maybe_stringified_schedule_arg(value)?
            && normalized.get("kind").and_then(Value::as_str) == Some("after")
        {
            return CronAddScheduleArg::after_from_value(&normalized);
        }

        deserialize_schedule_arg(value).map(CronAddScheduleArg::Schedule)
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

fn normalize_maybe_stringified_schedule_arg(value: &Value) -> Result<Option<Value>, String> {
    match value {
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') {
                serde_json::from_str(trimmed)
                    .map(Some)
                    .map_err(|err| format!("Invalid schedule: {err}"))
            } else {
                Ok(None)
            }
        }
        other => Ok(Some(other.clone())),
    }
}

enum CronAddScheduleArg {
    Schedule(Schedule),
    AfterSeconds(u64),
}

impl CronAddScheduleArg {
    fn after_from_value(value: &Value) -> Result<Self, String> {
        let after_seconds = value
            .get("after_seconds")
            .and_then(Value::as_u64)
            .ok_or_else(|| "Invalid schedule: after_seconds must be an integer > 0".to_string())?;
        if after_seconds == 0 {
            return Err("Invalid schedule: after_seconds must be > 0".to_string());
        }

        Ok(Self::AfterSeconds(after_seconds))
    }

    fn default_delete_after_run(&self) -> bool {
        matches!(
            self,
            Self::Schedule(Schedule::At { .. }) | Self::AfterSeconds(_)
        )
    }

    fn into_schedule(self) -> Result<Schedule, String> {
        match self {
            Self::Schedule(schedule) => Ok(schedule),
            Self::AfterSeconds(after_seconds) => {
                let after_seconds = i64::try_from(after_seconds)
                    .map_err(|_| "Invalid schedule: after_seconds is too large")?;
                let delay = ChronoDuration::seconds(after_seconds);
                let at = Utc::now().checked_add_signed(delay).ok_or_else(|| {
                    "Invalid schedule: after_seconds overflowed DateTime arithmetic".to_string()
                })?;
                Ok(Schedule::At { at })
            }
        }
    }
}

fn schedule_error_result(error: String) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(error),
    }
}

#[async_trait]
impl Tool for CronAddTool {
    fn name(&self) -> &str {
        "cron_add"
    }

    fn description(&self) -> &str {
        "Create a scheduled cron job (shell or agent) with cron/at/after/every schedules. \
         Use job_type='agent' with a prompt to run the AI agent on schedule. \
         For relative one-shot reminders such as 'in 10 minutes' or 'after 2 hours', \
         use schedule={\"kind\":\"after\",\"after_seconds\":...}; the runtime resolves it \
         with the live clock when the tool executes. \
         To deliver output to a channel (Discord, Telegram, Slack, Mattermost, Matrix, QQ, Webhook), set \
         delivery={\"mode\":\"announce\",\"channel\":\"discord\",\"to\":\"<channel_id_or_chat_id>\"}. \
         For webhook deliveries that must thread through the originating conversation, also set \
         delivery.thread_id=\"<reply_target>\". \
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
                    "description": "When to run the job. Exactly one of four forms must be used. Prefer 'after' for relative one-shot reminders.",
                    "oneOf": [
                        {
                            "type": "object",
                            "description": "Cron expression schedule (repeating). Example: {\"kind\":\"cron\",\"expr\":\"0 9 * * 1-5\",\"tz\":\"America/New_York\"}",
                            "properties": {
                                "kind": { "type": "string", "enum": ["cron"] },
                                "expr": { "type": "string", "description": "Standard 5-field cron expression, e.g. '*/5 * * * *'" },
                                "tz": { "type": "string", "description": CRON_TZ_DESCRIPTION }
                            },
                            "required": ["kind", "expr"]
                        },
                        {
                            "type": "object",
                            "description": "One-shot schedule at a specific RFC3339 timestamp with explicit Z or offset. Example: {\"kind\":\"at\",\"at\":\"2025-12-31T23:59:00Z\"}",
                            "properties": {
                                "kind": { "type": "string", "enum": ["at"] },
                                "at": { "type": "string", "description": AT_DESCRIPTION }
                            },
                            "required": ["kind", "at"]
                        },
                        {
                            "type": "object",
                            "description": "One-shot relative delay in seconds. Prefer this for reminders like 'in 10 minutes' so the runtime resolves the live clock. Example: {\"kind\":\"after\",\"after_seconds\":600}",
                            "properties": {
                                "kind": { "type": "string", "enum": ["after"] },
                                "after_seconds": { "type": "integer", "minimum": 1, "description": "Delay from job creation time in seconds, e.g. 600 for 10 minutes" }
                            },
                            "required": ["kind", "after_seconds"]
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
                    "description": "Optional allowlist of tool names for agent jobs. When omitted, cron-launched agent runs keep non-scheduler tools available but exclude scheduler mutation tools such as cron_add, cron_update, cron_remove, cron_run, and schedule. Include those names explicitly to opt back in."
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
                            "enum": ["telegram", "discord", "slack", "mattermost", "matrix", "qq", "webhook", "lark", "feishu"],
                            "description": "Channel type to deliver output to"
                        },
                        "to": {
                            "type": "string",
                            "description": "Destination ID: Discord channel ID, Telegram chat ID, Slack channel name, webhook recipient, etc."
                        },
                        "thread_id": {
                            "type": "string",
                            "description": "Optional thread/conversation identifier. Used by the webhook channel to route callbacks to the originating conversation; ignored by channels whose threading is implied by `to`."
                        },
                        "best_effort": {
                            "type": "boolean",
                            "description": "If true, a delivery failure does not fail the job itself. Defaults to true."
                        }
                    }
                },
                "delete_after_run": {
                    "type": "boolean",
                    "description": "If true, the job is automatically deleted after its first successful run. Defaults to true for one-shot 'at' and 'after' schedules."
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
        if !self.config.scheduler.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("cron is disabled by config (scheduler.enabled=false)".to_string()),
            });
        }

        let schedule_arg = match args.get("schedule") {
            Some(v @ serde_json::Value::String(raw)) => {
                if let Some(error) = Self::plain_string_schedule_error(raw) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                match Self::deserialize_cron_add_schedule_arg(v) {
                    Ok(schedule) => schedule,
                    Err(error) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(error),
                        });
                    }
                }
            }
            Some(v) => match Self::deserialize_cron_add_schedule_arg(v) {
                Ok(schedule) => schedule,
                Err(error) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
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

        let default_delete_after_run = schedule_arg.default_delete_after_run();
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

                let schedule = match schedule_arg.into_schedule() {
                    Ok(schedule) => schedule,
                    Err(error) => return Ok(schedule_error_result(error)),
                };

                cron::add_shell_job_with_approval(
                    &self.config,
                    &self.agent_alias,
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

                let schedule = match schedule_arg.into_schedule() {
                    Ok(schedule) => schedule,
                    Err(error) => return Ok(schedule_error_result(error)),
                };

                cron::add_agent_job(
                    &self.config,
                    &self.agent_alias,
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
                output: serde_json::to_string_pretty(&cron_add_output(&job))?,
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
    use crate::security::AutonomyLevel;
    use tempfile::TempDir;
    use zeroclaw_config::schema::Config;

    const TEST_AGENT: &str = "test-agent";

    async fn test_config(tmp: &TempDir) -> Arc<Config> {
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        Arc::new(config)
    }

    fn test_security(cfg: &Config) -> Arc<SecurityPolicy> {
        Arc::new(
            SecurityPolicy::for_agent(cfg, TEST_AGENT).expect("test-agent has resolvable profiles"),
        )
    }

    fn seed_test_agent(config: &mut Config) {
        config
            .risk_profiles
            .entry(TEST_AGENT.to_string())
            .or_default();
        config
            .runtime_profiles
            .entry(TEST_AGENT.to_string())
            .or_default();
        config
            .providers
            .models
            .ensure("openrouter", TEST_AGENT)
            .expect("known family");
        config.agents.entry(TEST_AGENT.to_string()).or_insert(
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: format!("openrouter.{TEST_AGENT}").into(),
                risk_profile: TEST_AGENT.to_string(),
                runtime_profile: TEST_AGENT.to_string(),
                ..Default::default()
            },
        );
    }

    #[tokio::test]
    async fn adds_shell_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
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
    async fn output_includes_timezone_confirmation_fields_for_explicit_cron_timezone() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "0 9 * * 1-5", "tz": "America/New_York" },
                "job_type": "shell",
                "command": "echo ok"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["next_run"], output["next_run_utc"]);
        assert_eq!(output["schedule_timezone"], "America/New_York");
        assert_eq!(output["timezone_source"], "explicit");
        assert!(
            output["next_run_local"]
                .as_str()
                .is_some_and(|value| value.contains("T09:00:00")),
            "next_run_local should display the next run in the explicit schedule timezone: {output}"
        );
    }

    #[tokio::test]
    async fn output_identifies_runtime_local_fallback_when_cron_timezone_is_omitted() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "echo ok"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["timezone_source"], "runtime_local");
        assert_eq!(output["schedule_timezone"], "runtime local timezone");
        assert!(
            output["next_run_local"].as_str().is_some(),
            "next_run_local should be present for runtime-local cron schedules: {output}"
        );
    }

    #[tokio::test]
    async fn shell_job_persists_delivery() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
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
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::Supervised;
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::ReadOnly;
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::Full;
        config
            .runtime_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .max_actions_per_hour = 0;
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["touch".into()];
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::Supervised;
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let cfg = Arc::new(config);
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn rejects_plain_string_schedule_with_actionable_error() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "schedule": "0 9 * * 1-5",
                "job_type": "shell",
                "command": "echo bad-schedule"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("expected a JSON object"));
        assert!(error.contains("\"kind\""));
        assert!(error.contains("plain string \"0 9 * * 1-5\""));
        assert!(error.contains("{\"kind\":\"cron\",\"expr\":\"0 9 * * 1-5\"}"));
        assert!(error.contains("{\"kind\":\"at\",\"at\":\"2025-12-31T23:59:00Z\"}"));
        assert!(error.contains("{\"kind\":\"every\",\"every_ms\":3600000}"));
        assert!(!error.contains("internally tagged enum"));
    }

    #[tokio::test]
    async fn accepts_stringified_interval_schedule() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn accepts_relative_after_schedule_as_one_shot_at() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let before = chrono::Utc::now();
        let result = tool
            .execute(json!({
                "schedule": { "kind": "after", "after_seconds": 60 },
                "job_type": "agent",
                "prompt": "remind me to drink water"
            }))
            .await
            .unwrap();
        let after = chrono::Utc::now();

        assert!(result.success, "{:?}", result.error);
        let jobs = cron::list_jobs(&cfg).unwrap();
        assert_eq!(jobs.len(), 1);
        match jobs[0].schedule {
            Schedule::At { at } => {
                assert!(at >= before + chrono::Duration::seconds(60));
                assert!(at <= after + chrono::Duration::seconds(60));
                assert_eq!(jobs[0].next_run, at);
            }
            ref other => {
                panic!("after input should persist as one-shot at schedule, got {other:?}")
            }
        }
        assert!(jobs[0].delete_after_run);

        let schema = tool.parameters_schema();
        let delete_description = schema["properties"]["delete_after_run"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(delete_description.contains("'at' and 'after' schedules"));
    }

    #[tokio::test]
    async fn accepts_stringified_relative_after_schedule() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let before = chrono::Utc::now();
        let result = tool
            .execute(json!({
                "schedule": r#"{"kind":"after","after_seconds":60}"#,
                "job_type": "agent",
                "prompt": "remind me to drink water"
            }))
            .await
            .unwrap();
        let after = chrono::Utc::now();

        assert!(result.success, "{:?}", result.error);
        let jobs = cron::list_jobs(&cfg).unwrap();
        match jobs[0].schedule {
            Schedule::At { at } => {
                assert!(at >= before + chrono::Duration::seconds(60));
                assert!(at <= after + chrono::Duration::seconds(60));
            }
            ref other => panic!("after input should persist as an at schedule, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_after_schedule_with_non_positive_delay() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "schedule": { "kind": "after", "after_seconds": 0 },
                "job_type": "agent",
                "prompt": "remind me"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("after_seconds must be > 0")
        );
    }

    #[tokio::test]
    async fn accepts_stringified_schedule_with_timezone() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn rejects_at_timestamp_without_explicit_offset_with_actionable_error() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "schedule": { "kind": "at", "at": "2026-05-18T09:00:00" },
                "job_type": "shell",
                "command": "echo at"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("RFC3339 timestamp with explicit Z or offset"),
            "error should explain the explicit offset requirement: {error}"
        );
        assert!(error.contains("2026-05-18T09:00:00Z"));
        assert!(error.contains("2026-05-18T09:00:00-04:00"));
    }

    #[tokio::test]
    async fn agent_job_requires_prompt() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn allowed_tools_schema_documents_scheduler_mutation_default() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let schema = tool.parameters_schema();
        let description = schema["properties"]["allowed_tools"]["description"]
            .as_str()
            .unwrap_or_default();

        assert!(description.contains("exclude scheduler mutation tools"));
        assert!(description.contains("cron_add"));
        assert!(description.contains("opt back in"));
    }

    #[tokio::test]
    async fn delivery_schema_includes_matrix_channel() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let values =
            tool.parameters_schema()["properties"]["delivery"]["properties"]["channel"]["enum"]
                .as_array()
                .cloned()
                .unwrap_or_default();

        assert!(values.iter().any(|value| value == "matrix"));
    }

    #[tokio::test]
    async fn delivery_schema_includes_webhook_and_thread_id() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let schema = tool.parameters_schema();

        let channel_enum = schema["properties"]["delivery"]["properties"]["channel"]["enum"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            channel_enum.iter().any(|value| value == "webhook"),
            "delivery.channel enum must include webhook"
        );

        let delivery_props = schema["properties"]["delivery"]["properties"]
            .as_object()
            .expect("delivery must have properties");
        assert!(
            delivery_props.contains_key("thread_id"),
            "delivery schema must expose thread_id so the webhook channel can route callbacks"
        );
    }

    #[tokio::test]
    async fn webhook_announce_job_persists_thread_id() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool
            .execute(json!({
                "schedule": { "kind": "cron", "expr": "*/5 * * * *" },
                "job_type": "shell",
                "command": "echo ok",
                "delivery": {
                    "mode": "announce",
                    "channel": "webhook",
                    "to": "user-42",
                    "thread_id": "conv-99",
                    "best_effort": true
                }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);

        let jobs = cron::list_jobs(&cfg).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].delivery.mode, "announce");
        assert_eq!(jobs[0].delivery.channel.as_deref(), Some("webhook"));
        assert_eq!(jobs[0].delivery.to.as_deref(), Some("user-42"));
        assert_eq!(jobs[0].delivery.thread_id.as_deref(), Some("conv-99"));
        assert!(jobs[0].delivery.best_effort);
    }

    #[tokio::test]
    async fn past_at_schedule_error_includes_clock_diagnostics() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronAddTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "schedule": { "kind": "at", "at": "2020-01-01T00:00:00Z" },
                "job_type": "shell",
                "command": "echo at"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("'at' must be in the future"));
        assert!(error.contains("now_utc="), "{error}");
        assert!(error.contains("now_local="), "{error}");
        assert!(
            error.contains("at_utc=2020-01-01T00:00:00+00:00"),
            "{error}"
        );
        assert!(error.contains("at_local="), "{error}");
        assert!(error.contains("delta_seconds="), "{error}");
    }

    #[test]
    fn schedule_schema_is_oneof_with_cron_at_every_variants() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            &cfg.data_dir,
        ));
        let tool = CronAddTool::new(cfg, security, TEST_AGENT);
        let schema = tool.parameters_schema();

        // Top-level: schedule is required
        let top_required = schema["required"].as_array().expect("top-level required");
        assert!(top_required.iter().any(|v| v == "schedule"));

        // schedule is a oneOf with four variants: cron, at, after, every
        let one_of = schema["properties"]["schedule"]["oneOf"]
            .as_array()
            .expect("schedule.oneOf must be an array");
        assert_eq!(
            one_of.len(),
            4,
            "expected cron, at, after, and every variants"
        );

        let kinds: Vec<&str> = one_of
            .iter()
            .filter_map(|v| v["properties"]["kind"]["enum"][0].as_str())
            .collect();
        assert!(kinds.contains(&"cron"), "missing cron variant");
        assert!(kinds.contains(&"at"), "missing at variant");
        assert!(kinds.contains(&"after"), "missing after variant");
        assert!(kinds.contains(&"every"), "missing every variant");

        // Each variant declares its required fields and duration fields are typed integers.
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
                "after" => {
                    assert!(
                        req.contains(&"after_seconds"),
                        "after variant missing 'after_seconds'"
                    );
                    assert_eq!(
                        variant["properties"]["after_seconds"]["type"].as_str(),
                        Some("integer"),
                        "after_seconds must be typed as integer"
                    );
                    assert_eq!(
                        variant["properties"]["after_seconds"]["minimum"].as_i64(),
                        Some(1),
                        "after_seconds must declare a positive minimum"
                    );
                }
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

        let cron_variant = one_of
            .iter()
            .find(|variant| variant["properties"]["kind"]["enum"][0] == "cron")
            .expect("cron variant");
        let cron_tz_description = cron_variant["properties"]["tz"]["description"]
            .as_str()
            .expect("cron tz description");
        assert!(
            cron_tz_description.contains("runtime local timezone"),
            "cron tz description must match scheduler fallback: {cron_tz_description}"
        );
        assert!(
            cron_tz_description.contains("explicit IANA timezone"),
            "cron tz description should recommend explicit IANA timezones: {cron_tz_description}"
        );
        assert!(
            !cron_tz_description.contains("Defaults to UTC"),
            "cron tz description must not claim a UTC default"
        );

        let at_variant = one_of
            .iter()
            .find(|variant| variant["properties"]["kind"]["enum"][0] == "at")
            .expect("at variant");
        let at_description = at_variant["properties"]["at"]["description"]
            .as_str()
            .expect("at description");
        assert!(
            at_description.contains("RFC3339 timestamp with explicit Z or offset"),
            "at description should require explicit Z or offset: {at_description}"
        );
    }
}
