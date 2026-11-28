use super::cron_common::{BoundOutput, bound_output, cron_job_output};
use crate::cron;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronListTool {
    config: Arc<Config>,
    /// Owning agent — jobs belonging to any other agent are not listed.
    agent_alias: String,
}

impl CronListTool {
    pub fn new(config: Arc<Config>, agent_alias: impl Into<String>) -> Self {
        Self {
            config,
            agent_alias: agent_alias.into(),
        }
    }

    /// The job as listed. A job bound to a conversation lists its last
    /// output only in that conversation; the store keeps no output from a
    /// bound run on a job that has lost its binding.
    fn job_view(&self, job: &cron::CronJob) -> serde_json::Result<serde_json::Value> {
        let mut view = cron_job_output(job)?;
        if job.last_output.is_some()
            && bound_output(&self.config, &job.id, &self.agent_alias) == BoundOutput::Withheld
            && let serde_json::Value::Object(ref mut object) = view
        {
            object.insert("last_output".to_string(), serde_json::Value::Null);
            object.insert("last_output_withheld".to_string(), json!(true));
        }
        Ok(view)
    }
}

#[async_trait]
impl Tool for CronListTool {
    fn requires_unrestricted_principal(&self) -> bool {
        // This path uses agent authority without carrying the session owner.
        true
    }

    fn name(&self) -> &str {
        "cron_list"
    }

    fn description(&self) -> &str {
        "List the scheduled cron jobs owned by this agent"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.config.scheduler.enabled {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("cron is disabled by config (scheduler.enabled=false)".to_string()),
            });
        }

        match cron::list_jobs_by_agent(&self.config, &self.agent_alias) {
            Ok(jobs) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(
                    &jobs
                        .iter()
                        .map(|job| self.job_view(job))
                        .collect::<serde_json::Result<Vec<_>>>()?,
                )?
                .into(),
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

    #[tokio::test]
    async fn returns_empty_list_when_no_jobs() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronListTool::new(cfg, TEST_AGENT);

        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output.trim(), "[]");
    }

    #[tokio::test]
    async fn output_includes_timezone_confirmation_fields_for_cron_jobs() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        cron::add_shell_job(
            &cfg,
            TEST_AGENT,
            None,
            cron::Schedule::Cron {
                expr: "0 9 * * 1-5".into(),
                tz: Some("America/New_York".into()),
            },
            "echo ok",
        )
        .unwrap();
        let tool = CronListTool::new(cfg, TEST_AGENT);

        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success, "{:?}", result.error);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let job = &output[0];
        assert_eq!(job["next_run"], job["next_run_utc"]);
        assert_eq!(job["schedule_timezone"], "America/New_York");
        assert_eq!(job["timezone_source"], "explicit");
        assert!(
            job["next_run_local"]
                .as_str()
                .is_some_and(|value| value.contains("T09:00:00")),
            "next_run_local should display the next run in the explicit schedule timezone: {job}"
        );
    }

    #[tokio::test]
    async fn errors_when_cron_disabled() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = (*test_config(&tmp).await).clone();
        cfg.scheduler.enabled = false;
        let tool = CronListTool::new(Arc::new(cfg), TEST_AGENT);

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("cron is disabled")
        );
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
    async fn lists_only_the_calling_agents_jobs() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let theirs = other_agents_job(&cfg);
        cron::add_job(&cfg, TEST_AGENT, "*/5 * * * *", "echo mine").unwrap();

        let tool = CronListTool::new(cfg.clone(), TEST_AGENT);
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        let rendered = format!("{:?}", result.output);
        assert!(rendered.contains("echo mine"), "own job must be listed");
        assert!(
            !rendered.contains(&theirs.id),
            "another agent's job must not be listed"
        );
    }

    fn telegram_binding(key: &str) -> zeroclaw_api::conversation_binding::ConversationBinding {
        zeroclaw_api::conversation_binding::ConversationBinding {
            surface: zeroclaw_api::conversation_binding::ConversationSurface::Channel,
            route: "telegram".to_string(),
            key: key.to_string(),
        }
    }

    fn serving(
        key: Option<&str>,
    ) -> Option<zeroclaw_api::conversation_binding::ActiveConversation> {
        key.map(
            |key| zeroclaw_api::conversation_binding::ActiveConversation {
                surface: zeroclaw_api::conversation_binding::ConversationSurface::Channel,
                route: "telegram".to_string(),
                key: key.to_string(),
                agent_alias: TEST_AGENT.to_string(),
            },
        )
    }

    fn bound_job(cfg: &Config) -> cron::CronJob {
        cron::add_agent_job_bound(
            cfg,
            TEST_AGENT,
            Some("reminder".into()),
            cron::Schedule::Every { every_ms: 60_000 },
            "Remind me to call Sam",
            cron::SessionTarget::Main,
            None,
            None,
            false,
            None,
            true,
            Some(&telegram_binding("telegram_42_42")),
        )
        .unwrap()
    }

    async fn listed_from(cfg: &Arc<Config>, key: Option<&str>) -> serde_json::Value {
        let tool = CronListTool::new(cfg.clone(), TEST_AGENT);
        let result = zeroclaw_api::TOOL_LOOP_ACTIVE_CONVERSATION
            .scope(serving(key), tool.execute(json!({})))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        serde_json::from_str::<serde_json::Value>(&result.output).unwrap()[0].clone()
    }

    #[tokio::test]
    async fn bound_job_lists_its_last_output_only_in_its_conversation() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let job = bound_job(&cfg);
        cron::record_last_run(&cfg, &job.id, chrono::Utc::now(), true, "Sam said six").unwrap();

        let inside = listed_from(&cfg, Some("telegram_42_42")).await;
        assert_eq!(inside["last_output"], "Sam said six");
        assert!(inside.get("last_output_withheld").is_none(), "{inside}");

        for outside in [Some("telegram_99_99"), None] {
            let seen = listed_from(&cfg, outside).await;
            assert_eq!(seen["last_output"], serde_json::Value::Null, "{seen}");
            assert_eq!(seen["last_output_withheld"], true, "{seen}");
            assert_eq!(seen["last_status"], "ok", "{seen}");
        }
    }
}
