use crate::cron::{self, JobType};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronRunTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
    /// Owning agent — another agent's job cannot be triggered from here.
    agent_alias: String,
    runtime: Arc<dyn RuntimeAdapter>,
    /// Bounded-delegation ceiling for the registering loop, or `None` when the
    /// registration is unbounded. Unlike the writing tools, this one launches a
    /// job whose tool set was stored earlier — possibly by the owning agent with
    /// no ceiling in force — so there is nothing left to intersect and an
    /// out-of-ceiling job is refused. See [`crate::tools::caller_ceiling`].
    caller_ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
}

impl CronRunTool {
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
            agent_alias: agent_alias.into(),
            runtime,
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
}

#[async_trait]
impl Tool for CronRunTool {
    fn name(&self) -> &str {
        "cron_run"
    }

    fn description(&self) -> &str {
        "Force-run a cron job immediately and record run history"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string" },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk shell commands in supervised mode",
                    "default": false
                }
            },
            "required": ["job_id"]
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

        let job_id = match args.get("job_id").and_then(serde_json::Value::as_str) {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'job_id' parameter".to_string()),
                });
            }
        };
        let approved = args
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Security policy: read-only mode, cannot perform 'cron_run'".into()),
            });
        }

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        let job = match cron::get_job_for_agent(&self.config, job_id, &self.agent_alias) {
            Ok(job) => job,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        // Launching an existing job runs its STORED tool set, which may predate
        // this bounded turn. Nothing is left to intersect at this point, so a
        // job that is not already within the ceiling is refused outright.
        //
        // The branch is load-bearing, and this is the verb that EXECUTES rather
        // than writes. A shell job's stored `allowed_tools` does not describe
        // what it runs — the command does, and the scheduler runs it under the
        // owning agent's policy with no tool gate anywhere on that path. Asking
        // `require_within_ceiling` about that list would bound a shell job by a
        // field it does not own: today such a job usually stores `None` and is
        // refused for the right outcome by accident, but the column is writable
        // by any unbounded turn of the owning agent (`cron/store.rs:592-599`
        // applies an `allowed_tools` patch without consulting `job_type`), so a
        // harmless-looking list turns the accident into a pass.
        let bounded = match job.job_type {
            JobType::Shell => crate::tools::caller_ceiling::require_shell_within_ceiling(
                "cron_run",
                self.caller_ceiling.as_ref(),
            ),
            JobType::Agent => crate::tools::caller_ceiling::require_within_ceiling(
                "cron_run",
                self.caller_ceiling.as_ref(),
                job.allowed_tools.as_deref(),
            ),
        };
        if let Err(error) = bounded {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        if matches!(job.job_type, JobType::Shell)
            && let Err(reason) = cron::validate_shell_command_with_security(
                self.runtime.as_ref(),
                &self.security,
                &job.command,
                approved,
            )
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(reason.to_string()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let result = cron::scheduler::run_manual_job_with_runtime(
            &self.config,
            &job,
            cron::scheduler::CronDeliveryContext::ToolManual,
            &None,
            self.runtime.as_ref(),
            approved,
        )
        .await;

        Ok(ToolResult {
            success: result.success,
            output: serde_json::to_string_pretty(&json!({
                "job_id": result.job_id,
                "status": result.status,
                "duration_ms": result.duration_ms,
                "output": result.output
            }))?
            .into(),
            error: if result.success {
                None
            } else {
                Some("cron job execution failed".to_string())
            },
        })
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

    // ── caller ceiling ──────────────────────────────────────────────────────

    fn sealed_ceiling(names: &[&str]) -> crate::tools::caller_ceiling::CallerCeiling {
        let handle: crate::tools::caller_ceiling::CallerCeiling =
            Arc::new(std::sync::OnceLock::new());
        let _ = handle.set(names.iter().map(|n| (*n).to_string()).collect());
        handle
    }

    fn bounded_tool(cfg: &Arc<Config>, ceiling: &[&str]) -> CronRunTool {
        let runtime = Arc::from(
            crate::platform::create_runtime(&cfg.runtime)
                .expect("test config must construct its runtime"),
        );
        CronRunTool::new_with_runtime(
            Arc::clone(cfg),
            test_security(cfg),
            TEST_AGENT,
            runtime,
            Some(sealed_ceiling(ceiling)),
        )
    }

    /// `cron_run` is the verb that EXECUTES, and a shell job's stored
    /// `allowed_tools` does not describe what it runs. Bounding it by that list
    /// gives the right answer only while the list is empty — and any unbounded
    /// turn of the owning agent can fill it, which is what this test does first
    /// through a public API rather than assuming the state.
    #[tokio::test]
    async fn a_bounded_caller_without_shell_cannot_force_run_a_shell_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        // The precondition, reached the way an ordinary unbounded turn reaches
        // it: `allowed_tools` is writable on a shell row, which turns a refusal
        // that held by accident into a pass.
        cron::update_shell_job_with_approval(
            &cfg,
            TEST_AGENT,
            &job.id,
            cron::CronJobPatch {
                allowed_tools: Some(vec!["cron_run".to_string()]),
                ..Default::default()
            },
            false,
        )
        .unwrap();

        let tool = bounded_tool(&cfg, &["cron_run"]);
        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();

        assert!(
            !result.success,
            "a bounded caller without `shell` force-ran a stored shell command: {result:?}"
        );
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("shell"),
            "the refusal must be the shell bound, not the stored-list one: {error}"
        );
        assert!(
            cron::list_runs(&cfg, &job.id, 10).unwrap().is_empty(),
            "the refusal must not have recorded a run"
        );
    }

    /// The positive half: the same job, the same stored list, one difference —
    /// `shell` is inside the ceiling. Without this the refusal above would be
    /// satisfied by a `cron_run` that refuses every bounded shell job.
    #[tokio::test]
    async fn a_bounded_caller_holding_shell_may_force_run_a_shell_job() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo run-now").unwrap();
        // `execute_job_now`'s reverse lookup needs the job on the agent, the
        // same wiring `force_runs_job_and_records_history` does.
        config
            .agents
            .get_mut(TEST_AGENT)
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let cfg = Arc::new(config);

        let tool = bounded_tool(&cfg, &["cron_run", "shell"]);
        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();

        assert!(
            result.success,
            "a caller holding `shell` may still force-run a shell job: {result:?}"
        );
    }

    /// A shell job is bounded by `shell`, NOT by whatever sits in its
    /// `allowed_tools` column — that column does not describe what a shell job
    /// runs. This is the other half of the branch: the list may reach well
    /// beyond the ceiling and the run still proceeds, because the caller could
    /// have run the command itself.
    ///
    /// It is also the test that catches the two arms being swapped: under a
    /// swap this job would be judged by its stored list and refused.
    #[tokio::test]
    async fn a_shell_job_is_not_bounded_by_its_stored_list_when_the_caller_held_shell() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo run-now").unwrap();
        config
            .agents
            .get_mut(TEST_AGENT)
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let cfg = Arc::new(config);
        cron::update_shell_job_with_approval(
            &cfg,
            TEST_AGENT,
            &job.id,
            cron::CronJobPatch {
                allowed_tools: Some(vec!["file_write".to_string()]),
                ..Default::default()
            },
            false,
        )
        .unwrap();

        let tool = bounded_tool(&cfg, &["cron_run", "shell"]);
        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();

        assert!(
            result.success,
            "a shell job must be judged by `shell`, not by a column it does not \
             own: {result:?}"
        );
    }

    /// The AGENT arm of the same branch, which the shell tests never reach.
    ///
    /// The ceiling deliberately contains `shell` here: under a swap of the two
    /// arms this job would be judged by `require_shell_within_ceiling`, find
    /// `shell` present and be allowed to run. The refusal below is what makes
    /// the arms non-interchangeable.
    ///
    /// Honest limit: its positive half lives in the unit tests of
    /// `caller_ceiling` (`launching_a_job_within_the_ceiling_is_allowed`), not
    /// here — an in-ceiling agent job would start a real agent turn, which
    /// needs a provider these tool tests do not stand up.
    #[tokio::test]
    async fn a_bounded_caller_cannot_force_run_an_agent_job_reaching_beyond_the_ceiling() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_agent_job(
            &cfg,
            TEST_AGENT,
            None,
            cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "do the scheduled work",
            cron::SessionTarget::Isolated,
            None,
            None,
            false,
            Some(vec!["file_write".to_string()]),
            true,
        )
        .unwrap();

        let tool = bounded_tool(&cfg, &["cron_run", "shell"]);
        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();

        assert!(
            !result.success,
            "an agent job storing a tool outside the ceiling must not launch: {result:?}"
        );
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("file_write"),
            "the refusal must name the offending stored tool: {error}"
        );
    }

    /// A ceiling in force but never sealed is an error, never an absent bound —
    /// asserted at the tool, not only at the predicate, because the wiring is
    /// what decides whether the tool consults it at all.
    #[tokio::test]
    async fn an_unsealed_ceiling_refuses_at_cron_run() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let runtime = Arc::from(
            crate::platform::create_runtime(&cfg.runtime)
                .expect("test config must construct its runtime"),
        );
        let unsealed: crate::tools::caller_ceiling::CallerCeiling =
            Arc::new(std::sync::OnceLock::new());
        let tool = CronRunTool::new_with_runtime(
            Arc::clone(&cfg),
            test_security(&cfg),
            TEST_AGENT,
            runtime,
            Some(unsealed),
        );

        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();

        assert!(!result.success, "{result:?}");
        assert!(
            result.error.unwrap_or_default().contains("never sealed"),
            "an unsealed ceiling must fail closed at the tool"
        );
        assert!(
            cron::list_runs(&cfg, &job.id, 10).unwrap().is_empty(),
            "the refusal must not have recorded a run"
        );
    }

    #[tokio::test]
    async fn force_runs_job_and_records_history() {
        let tmp = TempDir::new().unwrap();
        // Build the config so we can wire the imperative job's UUID
        // into test-agent's cron_jobs list before wrapping in Arc —
        // otherwise execute_job_now's reverse-lookup can't find the
        // owning agent.
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo run-now").unwrap();
        config
            .agents
            .get_mut(TEST_AGENT)
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let cfg = Arc::new(config);
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();
        assert!(result.success, "{:?}", result.error);

        let runs = cron::list_runs(&cfg, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
    }

    #[tokio::test]
    async fn best_effort_delivery_failure_records_degraded_history() {
        cron::scheduler::register_delivery_fn(Box::new(
            |_config, channel, _target, _thread_id, _output| {
                Box::pin(async move {
                    if channel == "fail-delivery" {
                        anyhow::bail!("synthetic delivery failure");
                    }
                    Ok(())
                })
            },
        ));

        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let job = cron::add_shell_job_with_approval(
            &config,
            TEST_AGENT,
            None,
            cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "echo run-now",
            Some(cron::DeliveryConfig {
                mode: "announce".into(),
                channel: Some("fail-delivery".into()),
                to: Some("123456".into()),
                thread_id: None,
                best_effort: true,
            }),
            true,
        )
        .unwrap();
        config
            .agents
            .get_mut(TEST_AGENT)
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let cfg = Arc::new(config);
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();
        assert!(result.success, "{:?}", result.error);
        let response: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(response["status"], "degraded");
        assert!(
            response["output"]
                .as_str()
                .unwrap_or_default()
                .contains("delivery failed:")
        );

        let updated = cron::get_job(&cfg, &job.id).unwrap();
        assert_eq!(updated.last_status.as_deref(), Some("degraded"));
        assert!(
            updated
                .last_output
                .as_deref()
                .unwrap_or_default()
                .contains("delivery failed:")
        );

        let runs = cron::list_runs(&cfg, &job.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "degraded");
        assert!(
            runs[0]
                .output
                .as_deref()
                .unwrap_or_default()
                .contains("delivery failed:")
        );
    }

    #[tokio::test]
    async fn errors_for_missing_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool
            .execute(json!({ "job_id": "missing-job-id" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("not found"));
    }

    #[tokio::test]
    async fn blocks_run_in_read_only_mode() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        seed_test_agent(&mut config);
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo run-now").unwrap();
        config
            .risk_profiles
            .entry(TEST_AGENT.into())
            .or_default()
            .level = AutonomyLevel::ReadOnly;
        let cfg = Arc::new(config);
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("read-only"));
    }

    #[tokio::test]
    async fn shell_run_requires_approval_for_medium_risk() {
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
            .allowed_commands = vec!["touch".into()];
        std::fs::create_dir_all(&config.data_dir).unwrap();
        seed_test_agent(&mut config);
        let cfg = Arc::new(config);
        // Create with explicit approval so the job persists for the run test.
        let job = cron::add_shell_job_with_approval(
            &cfg,
            TEST_AGENT,
            None,
            cron::Schedule::Cron {
                expr: "*/5 * * * *".into(),
                tz: None,
            },
            "touch cron-run-approval",
            None,
            true,
        )
        .unwrap();
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        // Without approval, the tool-level policy check blocks medium-risk commands.
        let denied = tool.execute(json!({ "job_id": job.id })).await.unwrap();
        assert!(!denied.success);
        assert!(
            denied
                .error
                .unwrap_or_default()
                .contains("explicit approval")
        );
    }

    #[tokio::test]
    async fn blocks_run_when_rate_limited() {
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
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo run-now").unwrap();
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Rate limit exceeded")
        );
        assert!(cron::list_runs(&cfg, &job.id, 10).unwrap().is_empty());
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
    async fn cannot_trigger_another_agents_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let theirs = other_agents_job(&cfg);

        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool.execute(json!({"job_id": theirs.id})).await.unwrap();

        assert!(!result.success);
        assert!(
            cron::list_runs(&cfg, &theirs.id, 10).unwrap().is_empty(),
            "another agent's job must not have been executed"
        );
    }
}
