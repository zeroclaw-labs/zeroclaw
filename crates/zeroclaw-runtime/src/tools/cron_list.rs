use super::cron_common::cron_job_output;
use crate::cron;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;

pub struct CronListTool {
    config: Arc<Config>,
}

impl CronListTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &str {
        "cron_list"
    }

    fn description(&self) -> &str {
        "List all scheduled cron jobs"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.config.cron.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("cron is disabled by config (cron.enabled=false)".to_string()),
            });
        }

        match cron::list_jobs(&self.config) {
            Ok(jobs) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(
                    &jobs
                        .iter()
                        .map(cron_job_output)
                        .collect::<serde_json::Result<Vec<_>>>()?,
                )?,
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
    use tempfile::TempDir;
    use zeroclaw_config::schema::Config;

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

    #[tokio::test]
    async fn returns_empty_list_when_no_jobs() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronListTool::new(cfg);

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
            None,
            cron::Schedule::Cron {
                expr: "0 9 * * 1-5".into(),
                tz: Some("America/New_York".into()),
            },
            "echo ok",
        )
        .unwrap();
        let tool = CronListTool::new(cfg);

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
        cfg.cron.enabled = false;
        let tool = CronListTool::new(Arc::new(cfg));

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("cron is disabled")
        );
    }
}
