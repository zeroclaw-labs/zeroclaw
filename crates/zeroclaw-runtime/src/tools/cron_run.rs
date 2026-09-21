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
}

struct ManualCronClaim {
    config: Arc<Config>,
    job_id: String,
    agent_alias: String,
    lock_token: String,
    released: bool,
}

impl ManualCronClaim {
    fn new(config: Arc<Config>, job_id: String, agent_alias: String, lock_token: String) -> Self {
        Self {
            config,
            job_id,
            agent_alias,
            lock_token,
            released: false,
        }
    }

    fn release(&mut self) {
        if self.released {
            return;
        }

        match cron::release_job_for_token(&self.config, &self.job_id, &self.lock_token) {
            Ok(_) => self.released = true,
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "job_id": self.job_id,
                        "agent_alias": self.agent_alias,
                        "error": format!("{e}")
                    })),
                "agent cron_run: failed to release in-flight lock"
            ),
        }
        // Once the manual run has finished (or cancellation has dropped this
        // guard), recovery must be allowed to clear the token even if both
        // release attempts fail.
        cron::finish_agent_claim(&self.config, &self.job_id, &self.lock_token);
    }
}

impl Drop for ManualCronClaim {
    fn drop(&mut self) {
        // Tool cancellation drops the execute future, so cleanup must live in
        // this guard instead of only after the awaited manual run completes.
        self.release();
    }
}

impl CronRunTool {
    pub fn new_with_runtime(
        config: Arc<Config>,
        security: Arc<SecurityPolicy>,
        agent_alias: impl Into<String>,
        runtime: Arc<dyn RuntimeAdapter>,
    ) -> Self {
        Self {
            config,
            security,
            agent_alias: agent_alias.into(),
            runtime,
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
        Self::new_with_runtime(config, security, agent_alias, runtime)
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

        let lock_token = match cron::claim_job_for_agent_with_token(
            &self.config,
            &job.id,
            &self.agent_alias,
            chrono::Utc::now(),
        ) {
            Ok(lock_token) => lock_token,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };
        let Some(lock_token) = lock_token else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Cron job '{job_id}' not found or is already in flight"
                )),
            });
        };

        let mut claim = ManualCronClaim::new(
            self.config.clone(),
            job.id.clone(),
            self.agent_alias.clone(),
            lock_token,
        );
        let result = cron::scheduler::run_manual_job_with_runtime(
            &self.config,
            &job,
            cron::scheduler::CronDeliveryContext::ToolManual,
            &None,
            self.runtime.as_ref(),
            approved,
        )
        .await;

        claim.release();

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
    use std::time::Duration;
    use tempfile::TempDir;
    use zeroclaw_api::runtime_traits::{RuntimeAdapter, ShellDialect};
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

    struct BlockingRuntime {
        started: Arc<tokio::sync::Notify>,
    }

    impl RuntimeAdapter for BlockingRuntime {
        fn name(&self) -> &str {
            "blocking-test-runtime"
        }

        fn has_filesystem_access(&self) -> bool {
            true
        }

        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }

        fn supports_long_running(&self) -> bool {
            true
        }

        fn shell_dialect(&self) -> ShellDialect {
            #[cfg(target_os = "windows")]
            {
                ShellDialect::WindowsCmd
            }
            #[cfg(not(target_os = "windows"))]
            {
                ShellDialect::Posix
            }
        }

        fn build_shell_command(
            &self,
            _command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            self.started.notify_one();

            #[cfg(target_os = "windows")]
            let mut command = {
                let mut command = tokio::process::Command::new("cmd");
                command.args(["/C", "ping", "-n", "60", "127.0.0.1", ">NUL"]);
                command
            };

            #[cfg(not(target_os = "windows"))]
            let mut command = {
                let mut command = tokio::process::Command::new("sleep");
                command.arg("60");
                command
            };

            command.current_dir(workspace_dir);
            Ok(command)
        }
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
        assert!(cron::claim_job_for_agent(&cfg, &job.id, TEST_AGENT, chrono::Utc::now()).unwrap());
        cron::release_job(&cfg, &job.id).unwrap();
    }

    #[tokio::test]
    async fn cancelled_run_releases_its_claim() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo blocking").unwrap();
        config
            .agents
            .get_mut(TEST_AGENT)
            .unwrap()
            .cron_jobs
            .push(job.id.clone());
        let cfg = Arc::new(config);
        let started = Arc::new(tokio::sync::Notify::new());
        let runtime = Arc::new(BlockingRuntime {
            started: started.clone(),
        });
        let tool =
            CronRunTool::new_with_runtime(cfg.clone(), test_security(&cfg), TEST_AGENT, runtime);

        let job_id = job.id.clone();
        let run =
            zeroclaw_spawn::spawn!(async move { tool.execute(json!({ "job_id": job_id })).await });
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .expect("manual run should start after claiming the job");
        assert!(!run.is_finished(), "manual run should still be pending");

        run.abort();
        assert!(run.await.unwrap_err().is_cancelled());

        assert!(
            cron::claim_job_for_agent(&cfg, &job.id, TEST_AGENT, chrono::Utc::now()).unwrap(),
            "a cancelled manual run must not leave its job locked"
        );
        cron::release_job(&cfg, &job.id).unwrap();
    }

    #[test]
    fn failed_manual_claim_release_is_recovered_in_the_same_process() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let job =
            cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo release-failure").unwrap();
        let config = Arc::new(config);
        let lock_token =
            cron::claim_job_for_agent_with_token(&config, &job.id, TEST_AGENT, chrono::Utc::now())
                .unwrap()
                .expect("manual claim should succeed");

        cron::force_release_failure_for_tests(&config, true);
        let mut claim = ManualCronClaim::new(
            config.clone(),
            job.id.clone(),
            TEST_AGENT.to_string(),
            lock_token,
        );
        claim.release();
        drop(claim);
        cron::force_release_failure_for_tests(&config, false);

        assert_eq!(
            cron::clear_stale_locks(&config).unwrap(),
            1,
            "same-process recovery must clear a terminated manual claim after release failure"
        );
        assert!(
            cron::claim_job_for_agent(&config, &job.id, TEST_AGENT, chrono::Utc::now()).unwrap(),
            "the recovered job must be claimable again"
        );
        cron::release_job(&config, &job.id).unwrap();
    }

    #[tokio::test]
    async fn refuses_to_run_a_job_after_ownership_moves() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_test_agent(&mut config);
        tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
        let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo run-now").unwrap();
        cron::rename_jobs_by_agent(&config, TEST_AGENT, "new-owner").unwrap();
        let cfg = Arc::new(config);
        let tool = CronRunTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({ "job_id": job.id })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("not found"));
        assert!(cron::list_runs(&cfg, &job.id, 10).unwrap().is_empty());
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
