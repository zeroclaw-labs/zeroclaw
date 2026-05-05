use crate::cron::{self, CronJobPatch, deserialize_maybe_stringified};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronUpdateTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
    /// Owning agent — risk profile gate for command updates.
    agent_alias: String,
}

impl CronUpdateTool {
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
impl Tool for CronUpdateTool {
    fn name(&self) -> &str {
        "cron_update"
    }

    fn description(&self) -> &str {
        "Patch an existing cron job (schedule, command, prompt, enabled, delivery, model, etc.)"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "ID of the cron job to update, as returned by cron_add or cron_list"
                },
                "patch": {
                    "type": "object",
                    "description": "Fields to update. Only include fields you want to change; omitted fields are left as-is.",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "New human-readable name for the job"
                        },
                        "enabled": {
                            "type": "boolean",
                            "description": "Enable or disable the job without deleting it"
                        },
                        "command": {
                            "type": "string",
                            "description": "New shell command (for shell jobs)"
                        },
                        "prompt": {
                            "type": "string",
                            "description": "New agent prompt (for agent jobs)"
                        },
                        "model": {
                            "type": "string",
                            "description": "Model override for agent jobs, e.g. 'x-ai/grok-4-1-fast'"
                        },
                        "allowed_tools": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional replacement allowlist of tool names for agent jobs"
                        },
                        "session_target": {
                            "type": "string",
                            "enum": ["isolated", "main"],
                            "description": "Agent session context: 'isolated' starts fresh each run, 'main' reuses the primary session"
                        },
                        "delete_after_run": {
                            "type": "boolean",
                            "description": "If true, delete the job automatically after its first successful run"
                        },
                        // NOTE: oneOf is correct for OpenAI-compatible APIs (including OpenRouter).
                        // Gemini does not support oneOf in tool schemas; if Gemini native tool calling
                        // is ever wired up, SchemaCleanr::clean_for_gemini must be applied before
                        // tool specs are sent. See src/tools/schema.rs.
                        "schedule": {
                            "description": "New schedule for the job. Exactly one of three forms must be used.",
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
                        "delivery": {
                            "type": "object",
                            "description": "Delivery config to send job output to a channel after each run. When provided, mode, channel, and to are all expected.",
                            "properties": {
                                "mode": {
                                    "type": "string",
                                    "enum": ["none", "announce"],
                                    "description": "'announce' sends output to the specified channel; 'none' disables delivery"
                                },
                                "channel": {
                                    "type": "string",
                                    "enum": ["telegram", "discord", "slack", "mattermost", "matrix"],
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
                        }
                    }
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk shell commands in supervised mode",
                    "default": false
                }
            },
            "required": ["job_id", "patch"]
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

        let job_id = match args.get("job_id").and_then(serde_json::Value::as_str) {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'job_id' parameter".to_string()),
                });
            }
        };

        let patch_val = match args.get("patch") {
            Some(v) => v.clone(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'patch' parameter".to_string()),
                });
            }
        };

        let patch = match deserialize_maybe_stringified::<CronJobPatch>(&patch_val) {
            Ok(patch) => patch,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Invalid patch payload: {e}")),
                });
            }
        };
        let approved = args
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if let Some(blocked) = self.enforce_mutation_allowed("cron_update") {
            return Ok(blocked);
        }

        match cron::update_shell_job_with_approval(
            &self.config,
            &self.agent_alias,
            job_id,
            patch,
            approved,
        ) {
            Ok(job) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&job)?,
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

    async fn test_config(tmp: &TempDir) -> Arc<Config> {
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.risk_profiles.insert(
            "default".to_string(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        config.providers.models.insert(
            "openrouter".to_string(),
            std::collections::HashMap::from([(
                "default".to_string(),
                zeroclaw_config::schema::ModelProviderConfig::default(),
            )]),
        );
        config.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::DelegateAgentConfig {
                model_provider: "openrouter.default".to_string(),
                risk_profile: "default".to_string(),
                ..Default::default()
            },
        );
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        Arc::new(config)
    }

    fn seed_test_agent(config: &mut Config) {
        config
            .risk_profiles
            .entry("default".to_string())
            .or_default();
        config
            .providers
            .models
            .entry("openrouter".to_string())
            .or_default()
            .entry("default".to_string())
            .or_default();
        config.agents.entry("test-agent".to_string()).or_insert(
            zeroclaw_config::schema::DelegateAgentConfig {
                model_provider: "openrouter.default".to_string(),
                risk_profile: "default".to_string(),
                ..Default::default()
            },
        );
    }

    fn test_security(cfg: &Config) -> Arc<SecurityPolicy> {
        Arc::new(
            SecurityPolicy::for_agent(cfg, "test-agent").unwrap_or_else(|_| {
                SecurityPolicy::from_risk_profile(
                    &zeroclaw_config::schema::RiskProfileConfig::default(),
                    &cfg.workspace_dir,
                )
            }),
        )
    }

    #[tokio::test]
    async fn updates_enabled_flag() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "enabled": false }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("\"enabled\": false"));
    }

    #[tokio::test]
    async fn blocks_disallowed_command_updates() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "command": "curl https://example.com" }
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
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        seed_test_agent(&mut config);
        let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .level = AutonomyLevel::ReadOnly;
        let cfg = Arc::new(config);
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "enabled": false }
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("read-only"));
    }

    #[tokio::test]
    async fn medium_risk_shell_update_requires_approval() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .level = AutonomyLevel::Supervised;
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into(), "touch".into()];
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        seed_test_agent(&mut config);
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let denied = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "command": "touch cron-update-approval-test" }
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
                "job_id": job.id,
                "patch": { "command": "touch cron-update-approval-test" },
                "approved": true
            }))
            .await
            .unwrap();
        assert!(approved.success, "{:?}", approved.error);
    }

    #[test]
    fn patch_schema_covers_all_cronjobpatch_fields_and_schedule_is_oneof() {
        let tmp = TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            &cfg.workspace_dir,
        ));
        let tool = CronUpdateTool::new(cfg, security, "test-agent");
        let schema = tool.parameters_schema();

        // Top-level: job_id and patch are required
        let top_required = schema["required"].as_array().expect("top-level required");
        let top_req_strs: Vec<&str> = top_required.iter().filter_map(|v| v.as_str()).collect();
        assert!(top_req_strs.contains(&"job_id"));
        assert!(top_req_strs.contains(&"patch"));

        // patch exposes all CronJobPatch fields
        let patch_props = schema["properties"]["patch"]["properties"]
            .as_object()
            .expect("patch must have a properties object");
        for field in &[
            "name",
            "enabled",
            "command",
            "prompt",
            "model",
            "allowed_tools",
            "session_target",
            "delete_after_run",
            "schedule",
            "delivery",
        ] {
            assert!(
                patch_props.contains_key(*field),
                "patch schema missing field: {field}"
            );
        }

        // patch.schedule is a oneOf with exactly 3 variants: cron, at, every
        let one_of = schema["properties"]["patch"]["properties"]["schedule"]["oneOf"]
            .as_array()
            .expect("patch.schedule.oneOf must be an array");
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
                _ => panic!("unexpected schedule kind: {kind}"),
            }
        }

        // patch.delivery.channel enum covers all supported channels
        let channel_enum = schema["properties"]["patch"]["properties"]["delivery"]["properties"]
            ["channel"]["enum"]
            .as_array()
            .expect("patch.delivery.channel must have an enum");
        let channel_strs: Vec<&str> = channel_enum.iter().filter_map(|v| v.as_str()).collect();
        for ch in &["telegram", "discord", "slack", "mattermost", "matrix"] {
            assert!(channel_strs.contains(ch), "delivery.channel missing: {ch}");
        }
    }

    #[tokio::test]
    async fn blocks_update_when_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .level = AutonomyLevel::Full;
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .max_actions_per_hour = 0;
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        seed_test_agent(&mut config);
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, "test-agent", "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "enabled": false }
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
        assert!(cron::get_job(&cfg, &job.id).unwrap().enabled);
    }

    #[tokio::test]
    async fn empty_allowed_tools_patch_stored_as_none() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &cfg,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "check status",
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            Some(vec!["file_read".into()]),
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "allowed_tools": [] }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            cron::get_job(&cfg, &job.id).unwrap().allowed_tools,
            None,
            "empty allowed_tools patch should clear to None"
        );
    }

    #[tokio::test]
    async fn updates_agent_allowed_tools() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &cfg,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "check status",
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), "test-agent");

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "allowed_tools": ["file_read", "web_search"] }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            cron::get_job(&cfg, &job.id).unwrap().allowed_tools,
            Some(vec!["file_read".into(), "web_search".into()])
        );
    }
}
