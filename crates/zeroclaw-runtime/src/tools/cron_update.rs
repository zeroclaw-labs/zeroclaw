use super::cron_common::{
    AT_DESCRIPTION, CRON_TZ_DESCRIPTION, cron_job_output, deserialize_patch_arg,
};
use crate::cron;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronUpdateTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    /// Owning agent — risk profile gate for command updates.
    agent_alias: String,
    /// Bounded-delegation ceiling for the registering loop, or `None` when the
    /// registration is unbounded. See [`crate::tools::caller_ceiling`].
    caller_ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
}

impl CronUpdateTool {
    pub fn new_with_runtime(
        config: Arc<Config>,
        security: Arc<SecurityPolicy>,
        agent_alias: impl Into<String>,
        runtime: Arc<dyn RuntimeAdapter>,
        caller_ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
    ) -> Self {
        Self {
            config,
            security,
            runtime,
            agent_alias: agent_alias.into(),
            caller_ceiling,
        }
    }

    #[cfg(test)]
    pub fn new(
        config: Arc<Config>,
        security: Arc<SecurityPolicy>,
        agent_alias: impl Into<String>,
    ) -> Self {
        let runtime = Arc::from(
            crate::platform::create_runtime(&config.runtime)
                .expect("test config must construct its runtime"),
        );
        Self::new_with_runtime(config, security, agent_alias, runtime, None)
    }

    fn enforce_mutation_allowed(&self, action: &str) -> Option<ToolResult> {
        if !self.security.can_act() {
            return Some(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Security policy: read-only mode, cannot perform '{action}'"
                )),
            });
        }

        if self.security.is_rate_limited() {
            return Some(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Rate limit exceeded: too many actions in the last hour".to_string()),
            });
        }

        if !self.security.record_action() {
            return Some(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Rate limit exceeded: action budget exhausted".to_string()),
            });
        }

        None
    }
}

/// True when the only thing a patch can do is REMOVE capability.
///
/// `schedule` exempts `pause` from the ceiling for exactly this reason, and
/// `cron_update` must not be stricter than its sibling: otherwise a bounded
/// target could be unable to turn OFF a job it was not allowed to turn on,
/// which protects nothing and takes away the one safe response to a job it
/// should not have.
///
/// Destructured exhaustively on purpose. A new `CronJobPatch` field must not
/// join the exempt set by default, so adding one breaks this build instead of
/// silently widening what a bounded turn may write.
fn patch_only_disables(patch: &cron::CronJobPatch) -> bool {
    let cron::CronJobPatch {
        enabled,
        schedule,
        command,
        prompt,
        name,
        delivery,
        model,
        session_target,
        delete_after_run,
        allowed_tools,
        uses_memory,
        shell_output_format,
    } = patch;
    *enabled == Some(false)
        && schedule.is_none()
        && command.is_none()
        && prompt.is_none()
        && name.is_none()
        && delivery.is_none()
        && model.is_none()
        && session_target.is_none()
        && delete_after_run.is_none()
        && allowed_tools.is_none()
        && uses_memory.is_none()
        && shell_output_format.is_none()
}

#[async_trait]
impl Tool for CronUpdateTool {
    fn name(&self) -> &str {
        "cron_update"
    }

    fn description(&self) -> &str {
        "Patch an existing cron job (schedule, command, prompt, enabled, delivery, model, etc.). Accepts job name or ID — no need to call cron_list first."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "ID or name of the cron job to update. Accepts either the UUID returned by cron_add/cron_list or the human-readable job name (case-insensitive). No need to call cron_list first."
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
                            "description": "New shell command for shell jobs, or agent prompt for agent jobs"
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
                        "uses_memory": {
                            "type": "boolean",
                            "description": "If true (default), recall and inject memory context before agent job runs. Set to false for stateless digest/report jobs.",
                            "default": true
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
                                    "pattern": cron::cron_delivery_channel_pattern(),
                                    "description": "Channel to deliver output to. Use '<type>.<alias>' (e.g. 'telegram.work'); a bare type resolves only while that type has one configured instance. Supported types: telegram, discord, slack, mattermost, matrix, qq, whatsapp, webhook, lark, feishu, dingtalk, wechat, signal, email. Unlike cron_add, this patch is never filled in from the current conversation."
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
                output: ToolOutput::default(),
                error: Some("cron is disabled by config (scheduler.enabled=false)".to_string()),
            });
        }

        let raw_id = match args.get("job_id").and_then(serde_json::Value::as_str) {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'job_id' parameter".to_string()),
                });
            }
        };

        let job_id_owned =
            match cron::resolve_job_id_or_name(&self.config, raw_id, &self.agent_alias) {
                Ok(id) => id,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(e.to_string()),
                    });
                }
            };
        let job_id = job_id_owned.as_str();

        let patch_val = match args.get("patch") {
            Some(v) => v.clone(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'patch' parameter".to_string()),
                });
            }
        };

        let mut patch = match deserialize_patch_arg(&patch_val) {
            Ok(patch) => patch,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
                });
            }
        };
        // A patch that sets `allowed_tools` rewrites the stored tool set, so it
        // is the same write `cron_add` performs and takes the same cap. An empty
        // patch list clears the field to `None` (unrestricted), which is why the
        // cap refuses an empty result instead of storing it: without that,
        // `allowed_tools: []` would remove the inherited limit outright.
        if patch.allowed_tools.is_some() {
            match crate::tools::caller_ceiling::cap_stored_allowed_tools(
                "cron_update",
                self.caller_ceiling.as_ref(),
                patch.allowed_tools.take(),
            ) {
                Ok(capped) => patch.allowed_tools = capped,
                Err(error) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(error),
                    });
                }
            }
        }
        // Capping the patch is not enough, and the note that used to stand
        // above — "a patch that leaves the field unset widens nothing; a job
        // already stored wider than the ceiling is caught at launch by
        // `cron_run`" — was false as a security claim. `cron_run` is the MANUAL
        // launch verb; the automatic scheduler reaches `run_agent_job` directly
        // and hands it the job's STORED list, so a job stored without one runs
        // the owning agent's full registry. And every other patched field is
        // applied unconditionally by `update_job_inner`, so a bounded turn could
        // re-point an existing unbounded job's `prompt` and re-arm its
        // `schedule` without ever naming `allowed_tools`. The bound therefore
        // belongs on the RESULTING job, which means reading the stored one.
        //
        // A patch that can only DISABLE is exempt, matching `schedule`, which
        // guards `resume` and leaves `pause` alone: refusing it would leave a
        // bounded target unable to switch off a job it may not switch on.
        if self.caller_ceiling.is_some() && !patch_only_disables(&patch) {
            let existing = match cron::get_job_for_agent(&self.config, job_id, &self.agent_alias) {
                Ok(job) => job,
                Err(error) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(error.to_string()),
                    });
                }
            };
            let resulting = patch
                .allowed_tools
                .clone()
                .or_else(|| existing.allowed_tools.clone());
            let bounded = match existing.job_type {
                // A shell job has no tool list to bound: the stored command is
                // what the scheduler runs, under the owning agent's policy.
                cron::JobType::Shell => crate::tools::caller_ceiling::require_shell_within_ceiling(
                    "cron_update",
                    self.caller_ceiling.as_ref(),
                ),
                cron::JobType::Agent => crate::tools::caller_ceiling::require_within_ceiling(
                    "cron_update",
                    self.caller_ceiling.as_ref(),
                    resulting.as_deref(),
                ),
            };
            if let Err(error) = bounded {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
                });
            }
        }

        let approved = args
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if let Some(blocked) = self.enforce_mutation_allowed("cron_update") {
            return Ok(blocked);
        }

        match cron::update_shell_job_with_runtime(
            &self.config,
            self.runtime.as_ref(),
            &self.security,
            Some(self.agent_alias.as_str()),
            job_id,
            patch,
            approved,
        ) {
            Ok(job) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&cron_job_output(&job)?)?.into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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
                risk_profile: TEST_AGENT.into(),
                runtime_profile: TEST_AGENT.into(),
                ..Default::default()
            },
        );
    }

    fn test_security(cfg: &Config) -> Arc<SecurityPolicy> {
        Arc::new(
            SecurityPolicy::for_agent(cfg, TEST_AGENT).expect("test-agent has resolvable profiles"),
        )
    }

    #[tokio::test]
    async fn updates_enabled_flag() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn output_includes_timezone_confirmation_fields_for_explicit_cron_timezone() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": {
                    "schedule": {
                        "kind": "cron",
                        "expr": "0 9 * * 1-5",
                        "tz": "America/New_York"
                    }
                }
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
    async fn blocks_disallowed_command_updates() {
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
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn command_update_uses_injected_runtime_dialect() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        let risk_profile = config.risk_profiles.entry(TEST_AGENT.into()).or_default();
        risk_profile.level = AutonomyLevel::Full;
        risk_profile.allowed_commands = vec!["*".into()];
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(crate::platform::NativeRuntime::with_shell("pwsh".into()));
        let tool = CronUpdateTool::new_with_runtime(
            cfg.clone(),
            test_security(&cfg),
            TEST_AGENT,
            runtime,
            None,
        );

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "command": "ac blocked.txt value" },
                "approved": true
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("high-risk")),
            "{:?}",
            result.error
        );
        assert_eq!(cron::get_job(&cfg, &job.id).unwrap().command, "echo ok");
    }

    #[tokio::test]
    async fn blocks_mutation_in_read_only_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        seed_test_agent(&mut config);
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::ReadOnly;
        let cfg = Arc::new(config);
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::Supervised;
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .allowed_commands = vec!["echo".into(), "touch".into()];
        std::fs::create_dir_all(&config.data_dir).unwrap();
        seed_test_agent(&mut config);
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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

    #[tokio::test]
    async fn rejects_at_timestamp_without_explicit_offset_with_actionable_error() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": {
                    "schedule": {
                        "kind": "at",
                        "at": "2026-05-18T09:00:00"
                    }
                }
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

    #[test]
    fn patch_schema_covers_all_cronjobpatch_fields_and_schedule_is_oneof() {
        let tmp = TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            &cfg.data_dir,
        ));
        let tool = CronUpdateTool::new(cfg, security, TEST_AGENT);
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
        assert_eq!(
            patch_props["command"]["description"].as_str(),
            Some("New shell command for shell jobs, or agent prompt for agent jobs"),
            "command description must document the agent-job compatibility mapping"
        );

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

        // patch.delivery.channel admits every supported type AND the composite key.
        // cron_update is the case that needs the alias form: this patch is never
        // filled in from the conversation, so a composite key is the only
        // unambiguous way to name one instance in a multi-instance setup.
        let pattern = schema["properties"]["patch"]["properties"]["delivery"]["properties"]
            ["channel"]["pattern"]
            .as_str()
            .expect("patch.delivery.channel must declare a pattern")
            .to_string();
        let channel = regex::Regex::new(&pattern).expect("channel pattern must compile");
        for supported in cron::CRON_DELIVERY_SCHEMA_CHANNELS {
            assert!(channel.is_match(supported), "{supported} must be valid");
        }
        assert!(channel.is_match("dingtalk"));
        assert!(channel.is_match("wechat"));
        assert!(channel.is_match("signal"));
        assert!(channel.is_match("email"));
        assert!(
            channel.is_match("telegram.work"),
            "cron_update must accept the aliased form its description recommends"
        );
        assert!(!channel.is_match("sms"));

        // patch.delivery exposes thread_id so the webhook channel can route callbacks
        // back to the originating conversation.
        let delivery_props = schema["properties"]["patch"]["properties"]["delivery"]["properties"]
            .as_object()
            .expect("patch.delivery must have properties");
        assert!(
            delivery_props.contains_key("thread_id"),
            "patch.delivery missing thread_id"
        );
    }

    #[test]
    fn add_and_update_delivery_channel_schemas_match() {
        let tmp = TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let security = Arc::new(SecurityPolicy::from_risk_profile(
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            &cfg.data_dir,
        ));
        let add_tool = crate::tools::cron_add::CronAddTool::new(
            Arc::clone(&cfg),
            Arc::clone(&security),
            TEST_AGENT,
        );
        let update_tool = CronUpdateTool::new(cfg, security, TEST_AGENT);
        let add_schema = add_tool.parameters_schema();
        let update_schema = update_tool.parameters_schema();

        let add_pattern = add_schema["properties"]["delivery"]["properties"]["channel"]["pattern"]
            .as_str()
            .expect("cron_add delivery.channel must declare a pattern");
        let update_pattern =
            update_schema["properties"]["patch"]["properties"]["delivery"]["properties"]["channel"]
                ["pattern"]
                .as_str()
                .expect("cron_update patch.delivery.channel must declare a pattern");

        // Both tools must describe the same channel surface, or a value the model
        // learns from one becomes invalid in the other.
        assert_eq!(add_pattern, update_pattern);
        let channel = regex::Regex::new(update_pattern).expect("channel pattern must compile");
        for supported in cron::CRON_DELIVERY_SCHEMA_CHANNELS {
            assert!(channel.is_match(supported), "{supported} must be valid");
        }
        assert!(channel.is_match("telegram.work"));
        assert!(channel.is_match("dingtalk"));
    }

    #[tokio::test]
    async fn blocks_update_when_rate_limited() {
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
        seed_test_agent(&mut config);
        let cfg = Arc::new(config);
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
            TEST_AGENT,
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
            true,
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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
    async fn command_patch_on_agent_job_updates_prompt_without_shell_policy() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        let risk_profile = config.risk_profiles.entry(TEST_AGENT.into()).or_default();
        risk_profile.level = AutonomyLevel::Supervised;
        risk_profile.allowed_commands = vec!["echo".into()];
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let cfg = Arc::new(config);
        let job = cron::add_agent_job(
            &cfg,
            TEST_AGENT,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "old prompt",
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "command": "curl https://example.com" }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let updated = cron::get_job(&cfg, &job.id).unwrap();
        assert_eq!(updated.prompt.as_deref(), Some("curl https://example.com"));
        assert_eq!(
            updated.command, "",
            "agent jobs must not persist patch.command on the unused command column"
        );
    }

    #[tokio::test]
    async fn updates_agent_allowed_tools() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &cfg,
            TEST_AGENT,
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
            true,
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

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

    // ── caller ceiling ──────────────────────────────────────────────────────

    fn sealed_ceiling(names: &[&str]) -> crate::tools::caller_ceiling::CallerCeiling {
        let handle: crate::tools::caller_ceiling::CallerCeiling =
            Arc::new(std::sync::OnceLock::new());
        let _ = handle.set(names.iter().map(|n| (*n).to_string()).collect());
        handle
    }

    /// `CronUpdateTool` as the bounded assembly builds it: same construction as
    /// the `#[cfg(test)] new` above, with a sealed ceiling instead of `None`.
    fn bounded_tool(cfg: &Arc<Config>, ceiling: &[&str]) -> CronUpdateTool {
        let runtime = Arc::from(
            crate::platform::create_runtime(&cfg.runtime)
                .expect("test config must construct its runtime"),
        );
        CronUpdateTool::new_with_runtime(
            Arc::clone(cfg),
            test_security(cfg),
            TEST_AGENT,
            runtime,
            Some(sealed_ceiling(ceiling)),
        )
    }

    fn agent_job_with(
        cfg: &Config,
        prompt: &str,
        allowed: Option<Vec<String>>,
    ) -> crate::cron::CronJob {
        cron::add_agent_job(
            cfg,
            TEST_AGENT,
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            prompt,
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            allowed,
            true,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_patch_that_never_names_allowed_tools_cannot_re_arm_an_unbounded_job() {
        // The escape the `allowed_tools`-only guard left open: every other
        // patched field is applied unconditionally, so re-pointing an existing
        // job's prompt runs the owning agent's full registry on attacker text
        // at the next scheduler tick. Broken state: capping only the patch.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = agent_job_with(&cfg, "original work", None);
        let tool = bounded_tool(&cfg, &["cron_update", "cron_add"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "prompt": "do the attacker's work instead" }
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a bounded turn re-pointed a job stored with no ceiling: {result:?}"
        );
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("stores no allowed_tools"),
            "the refusal must name why an unset list is not a pass: {error}"
        );
        assert_eq!(
            cron::get_job(&cfg, &job.id).unwrap().prompt.as_deref(),
            Some("original work"),
            "the refusal must not have written anything"
        );
    }

    #[tokio::test]
    async fn a_patch_to_a_job_already_within_the_ceiling_still_applies() {
        // The positive half. Without it, a guard that refused every bounded
        // `cron_update` would satisfy the test above and break the tool.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = agent_job_with(&cfg, "original work", Some(vec!["cron_add".into()]));
        let tool = bounded_tool(&cfg, &["cron_update", "cron_add"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "prompt": "refined work" }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            cron::get_job(&cfg, &job.id).unwrap().prompt.as_deref(),
            Some("refined work")
        );
    }

    #[tokio::test]
    async fn narrowing_a_job_into_the_ceiling_is_still_allowed() {
        // The bound is on the RESULTING job, not on the stored one: a patch
        // that brings an unbounded job inside the ceiling must pass, or the
        // guard would make an out-of-ceiling job permanently unpatchable.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = agent_job_with(&cfg, "original work", None);
        let tool = bounded_tool(&cfg, &["cron_update", "cron_add"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "allowed_tools": ["cron_add"], "prompt": "refined work" }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let stored = cron::get_job(&cfg, &job.id).unwrap();
        assert_eq!(stored.allowed_tools, Some(vec!["cron_add".to_string()]));
        assert_eq!(stored.prompt.as_deref(), Some("refined work"));
    }

    #[tokio::test]
    async fn a_shell_job_cannot_be_re_armed_when_the_caller_lacks_shell() {
        // A shell job has no `allowed_tools` to bound: what runs is the stored
        // command, under the owning agent's policy. Re-enabling one is the same
        // deferred execution as creating it.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        // Disabled FIRST, so the patch below genuinely re-arms. Patching
        // `enabled: true` onto an already-enabled job would assert the refusal
        // without the scenario the name promises.
        cron::pause_job_for_agent(&cfg, &job.id, TEST_AGENT).unwrap();
        let tool = bounded_tool(&cfg, &["cron_update", "cron_add"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "enabled": true }
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a bounded turn re-armed a shell job its caller could not have run: {result:?}"
        );
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("shell"),
            "the refusal must name the capability it protects: {error}"
        );
        assert!(
            !cron::get_job(&cfg, &job.id).unwrap().enabled,
            "the refusal must not have re-armed the job"
        );
    }

    #[tokio::test]
    async fn a_shell_job_is_patchable_when_the_caller_held_shell() {
        // The positive half of the refusal above. It patches `name` rather than
        // `enabled: false`, which is exempt from the ceiling entirely: an exempt
        // field would make this pass without ever reaching the guard's allow
        // path, which is the branch it exists to prove.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = bounded_tool(&cfg, &["cron_update", "shell"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "name": "renamed-by-bounded-caller" }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            cron::get_job(&cfg, &job.id).unwrap().name.as_deref(),
            Some("renamed-by-bounded-caller")
        );
    }

    #[tokio::test]
    async fn a_bounded_caller_may_disable_a_shell_job_it_could_not_have_armed() {
        // Refusing this would protect nothing and remove the one safe response
        // to a job the target should not have: `schedule` already exempts
        // `pause` on the same reasoning, and the two must not disagree.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = bounded_tool(&cfg, &["cron_update", "cron_add"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "enabled": false }
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "a bounded caller must be able to switch OFF a shell job: {result:?}"
        );
        assert!(!cron::get_job(&cfg, &job.id).unwrap().enabled);
    }

    #[tokio::test]
    async fn the_disable_exemption_does_not_carry_a_second_field() {
        // The exemption is for a patch whose ONLY effect is removing
        // capability. Pairing it with any other field must fall back to the
        // guard, or `{"enabled": false, "command": "..."}` would be a way in.
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = bounded_tool(&cfg, &["cron_update", "cron_add"]);

        let result = tool
            .execute(json!({
                "job_id": job.id,
                "patch": { "enabled": false, "command": "echo smuggled" }
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a disable paired with another field must not ride the exemption: {result:?}"
        );
        assert_eq!(
            cron::get_job(&cfg, &job.id).unwrap().command,
            "echo ok",
            "the refusal must not have written the command"
        );
    }

    #[tokio::test]
    async fn accepts_job_name_without_prior_cron_list() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        cron::add_shell_job(
            &cfg,
            TEST_AGENT,
            Some("morning_briefing".into()),
            crate::cron::Schedule::Cron {
                expr: "0 7 * * 1-5".into(),
                tz: None,
            },
            "echo ok",
        )
        .unwrap();
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "job_id": "morning_briefing",
                "patch": { "enabled": false }
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("\"enabled\": false"));
    }

    #[tokio::test]
    async fn errors_on_unknown_name() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({
                "job_id": "no_such_job",
                "patch": { "enabled": false }
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("no_such_job"),);
    }

    /// A job owned by someone else. An agent job needs no risk profile for its
    /// owner, which keeps the fixture to the ownership boundary.
    fn other_agents_job(cfg: &Config) -> crate::cron::CronJob {
        cron::add_agent_job(
            cfg,
            "other-agent",
            Some("secret_job".into()),
            crate::cron::Schedule::Cron {
                expr: "0 8 * * *".into(),
                tz: None,
            },
            "read the other agent's inbox",
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            true,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn cannot_update_another_agents_job_by_id() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let theirs = other_agents_job(&cfg);

        let tool = CronUpdateTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool
            .execute(json!({"job_id": theirs.id, "patch": {"enabled": false}}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            cron::get_job(&cfg, &theirs.id).unwrap().enabled,
            "other agent's job must be untouched"
        );
    }
}
