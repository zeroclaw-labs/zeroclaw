use super::traits::{Tool, ToolResult, enforce_security_policy};
use crate::config::Config;
use crate::cron::{
    self, CronJobPatch, delivery_json_schema, deserialize_maybe_stringified, schedule_json_schema,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct CronUpdateTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl CronUpdateTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
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
            "additionalProperties": false,
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
                        "schedule": schedule_json_schema(),
                        "delivery": delivery_json_schema()
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
        if !self.config.cron.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("cron is disabled by config (cron.enabled=false)".to_string()),
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

        if let Some(blocked) = enforce_security_policy(&self.security, "cron_update") {
            return Ok(blocked);
        }

        match cron::update_shell_job_with_approval(&self.config, job_id, patch, approved) {
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
    async fn updates_enabled_flag() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
    async fn updates_prompt_of_agent_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &cfg,
            Some("agent-job".into()),
            cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "original prompt",
            cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "prompt": "updated prompt" }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let updated = cron::get_job(&cfg, &job.id).unwrap();
        assert_eq!(updated.prompt.as_deref(), Some("updated prompt"));
    }

    #[tokio::test]
    async fn blocks_disallowed_command_updates() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.autonomy.allowed_commands = vec!["echo".into()];
        tokio::fs::create_dir_all(&config.workspace_dir)
            .await
            .unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
        let job = cron::add_job(&config, "*/5 * * * *", "echo ok").unwrap();
        config.autonomy.level = AutonomyLevel::ReadOnly;
        let cfg = Arc::new(config);
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
        config.autonomy.level = AutonomyLevel::Supervised;
        config.autonomy.allowed_commands = vec!["echo".into(), "touch".into()];
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
    fn patch_schema_covers_all_cronjobpatch_fields_and_schedule_is_flat() {
        let tmp = TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let security = Arc::new(SecurityPolicy::from_config(
            &cfg.autonomy,
            &cfg.workspace_dir,
        ));
        let tool = CronUpdateTool::new(cfg, security);
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

        // patch.schedule is a flat object (no oneOf)
        let sched = &schema["properties"]["patch"]["properties"]["schedule"];
        assert!(sched.get("oneOf").is_none(), "schedule must NOT use oneOf");
        assert_eq!(sched["type"], "object");

        let kind_enum = sched["properties"]["kind"]["enum"]
            .as_array()
            .expect("kind.enum");
        let kinds: Vec<&str> = kind_enum.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(kinds, vec!["cron", "at", "every"]);

        // All schedule fields present
        let sched_props = sched["properties"]
            .as_object()
            .expect("schedule.properties");
        for field in &["kind", "expr", "tz", "at", "every_ms"] {
            assert!(
                sched_props.contains_key(*field),
                "schedule missing: {field}"
            );
        }
        assert_eq!(
            sched_props["every_ms"]["type"].as_str(),
            Some("integer"),
            "every_ms must be typed as integer"
        );

        // patch.delivery.channel enum covers all supported channels including qq
        let channel_enum = schema["properties"]["patch"]["properties"]["delivery"]["properties"]
            ["channel"]["enum"]
            .as_array()
            .expect("patch.delivery.channel must have an enum");
        let channel_strs: Vec<&str> = channel_enum.iter().filter_map(|v| v.as_str()).collect();
        for ch in &["telegram", "discord", "slack", "mattermost", "matrix", "qq"] {
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
        config.autonomy.level = AutonomyLevel::Full;
        config.autonomy.max_actions_per_hour = 0;
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg));

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
