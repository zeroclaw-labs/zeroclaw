use crate::cron;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronRemoveTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
    /// Owning agent — scopes name resolution to this agent's own jobs.
    agent_alias: String,
}

impl CronRemoveTool {
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
impl Tool for CronRemoveTool {
    fn name(&self) -> &str {
        "cron_remove"
    }

    fn description(&self) -> &str {
        "Remove a cron job by id or name"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "ID or name of the cron job to remove. Accepts either the UUID returned by cron_add/cron_list or the human-readable job name (case-insensitive). No need to call cron_list first."
                }
            },
            "required": ["job_id"]
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

        let raw_id = match args.get("job_id").and_then(serde_json::Value::as_str) {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'job_id' parameter".to_string()),
                });
            }
        };

        let job_id = match cron::resolve_job_id_or_name(&self.config, raw_id, &self.agent_alias) {
            Ok(id) => id,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        if let Some(blocked) = self.enforce_mutation_allowed("cron_remove") {
            return Ok(blocked);
        }

        match cron::remove_job(&self.config, &job_id) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Removed cron job {job_id}"),
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
    const OTHER_AGENT: &str = "other-agent";

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
        seed_agent(config, TEST_AGENT);
    }

    fn seed_agent(config: &mut Config, alias: &str) {
        config.risk_profiles.entry(alias.to_string()).or_default();
        config
            .runtime_profiles
            .entry(alias.to_string())
            .or_default();
        config
            .providers
            .models
            .ensure("openrouter", alias)
            .expect("known family");
        config.agents.entry(alias.to_string()).or_insert(
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: format!("openrouter.{alias}").into(),
                risk_profile: alias.into(),
                runtime_profile: alias.into(),
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
    async fn removes_existing_job() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();
        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({"job_id": job.id})).await.unwrap();
        assert!(result.success);
        assert!(cron::list_jobs(&cfg).unwrap().is_empty());
    }

    #[tokio::test]
    async fn errors_when_job_id_missing() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Missing 'job_id'")
        );
    }

    #[tokio::test]
    async fn blocks_remove_in_read_only_mode() {
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
        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({"job_id": job.id})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("read-only"));
    }

    #[tokio::test]
    async fn blocks_remove_when_rate_limited() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
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
        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({"job_id": job.id})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Rate limit exceeded")
        );
        assert_eq!(cron::list_jobs(&cfg).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn removes_job_by_name() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        cron::add_shell_job(
            &cfg,
            TEST_AGENT,
            Some("daily_sync".into()),
            crate::cron::Schedule::Cron {
                expr: "0 8 * * *".into(),
                tz: None,
            },
            "echo ok",
        )
        .unwrap();
        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);

        let result = tool.execute(json!({"job_id": "daily_sync"})).await.unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(cron::list_jobs(&cfg).unwrap().is_empty());
    }

    fn two_agent_config(tmp: &TempDir) -> Arc<Config> {
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        seed_agent(&mut config, TEST_AGENT);
        seed_agent(&mut config, OTHER_AGENT);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        Arc::new(config)
    }

    fn named_job(cfg: &Config, agent: &str, name: &str, command: &str) -> crate::cron::CronJob {
        cron::add_shell_job(
            cfg,
            agent,
            Some(name.into()),
            crate::cron::Schedule::Cron {
                expr: "0 8 * * *".into(),
                tz: None,
            },
            command,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn name_resolution_is_scoped_to_owning_agent() {
        // Both agents own a job named `daily_sync`. Removing by name from the
        // tool scoped to TEST_AGENT must hit only TEST_AGENT's job — the other
        // agent's identically-named job must neither be removed nor trigger a
        // false "ambiguous" error.
        let tmp = TempDir::new().unwrap();
        let cfg = two_agent_config(&tmp);
        let mine = named_job(&cfg, TEST_AGENT, "daily_sync", "echo mine");
        let theirs = named_job(&cfg, OTHER_AGENT, "daily_sync", "echo theirs");

        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool.execute(json!({"job_id": "daily_sync"})).await.unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(
            cron::get_job(&cfg, &mine.id).is_err(),
            "caller's own job should be removed"
        );
        assert!(
            cron::get_job(&cfg, &theirs.id).is_ok(),
            "other agent's same-named job must survive"
        );
    }

    #[tokio::test]
    async fn cannot_resolve_another_agents_job_by_name() {
        // Only OTHER_AGENT owns `secret_job`; a tool scoped to TEST_AGENT must
        // not be able to resolve (and thus remove) it by name.
        let tmp = TempDir::new().unwrap();
        let cfg = two_agent_config(&tmp);
        let theirs = named_job(&cfg, OTHER_AGENT, "secret_job", "echo secret");

        let tool = CronRemoveTool::new(cfg.clone(), test_security(&cfg), TEST_AGENT);
        let result = tool.execute(json!({"job_id": "secret_job"})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("No cron job found"),
            "another agent's job must be unresolvable by name"
        );
        assert!(
            cron::get_job(&cfg, &theirs.id).is_ok(),
            "other agent's job must be untouched"
        );
    }
}
