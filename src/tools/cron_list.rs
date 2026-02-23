use super::traits::{Tool, ToolResult};
use crate::config::Config;
use crate::cron;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

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
                output: serde_json::to_string_pretty(&jobs)?,
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
    async fn errors_when_cron_disabled() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = (*test_config(&tmp).await).clone();
        cfg.cron.enabled = false;
        let tool = CronListTool::new(Arc::new(cfg));

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("cron is disabled"));
    }

    #[test]
    fn schema_is_valid_object_with_no_required_properties() {
        let tmp = TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let tool = CronListTool::new(cfg);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].is_object());
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn name_returns_cron_list() {
        let tmp = TempDir::new().unwrap();
        let cfg = Arc::new(Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        });
        let tool = CronListTool::new(cfg);
        assert_eq!(tool.name(), "cron_list");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn lists_added_jobs() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        cron::add_job(&cfg, "*/5 * * * *", "echo alpha").unwrap();
        cron::add_job(&cfg, "0 * * * *", "echo beta").unwrap();

        let tool = CronListTool::new(cfg);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("alpha"));
        assert!(result.output.contains("beta"));
    }

    #[tokio::test]
    async fn accepts_extra_args_gracefully() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_config(&tmp).await;
        let tool = CronListTool::new(cfg);

        let result = tool
            .execute(json!({"unexpected_key": "ignored_value"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output.trim(), "[]");
    }
}
