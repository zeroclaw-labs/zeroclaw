use super::traits::{Tool, ToolResult};
use crate::config::Config;
use crate::migration::{migrate_openclaw, OpenClawMigrationOptions};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct OpenClawMigrationTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl OpenClawMigrationTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }

    fn require_write_access(&self) -> Option<ToolResult> {
        if !self.security.can_act() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        None
    }

    fn parse_optional_path(args: &Value, field: &str) -> anyhow::Result<Option<PathBuf>> {
        let Some(raw_value) = args.get(field) else {
            return Ok(None);
        };
        if raw_value.is_null() {
            return Ok(None);
        }

        let raw = raw_value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a string path"))?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(trimmed)))
    }

    fn parse_bool(args: &Value, field: &str, default: bool) -> anyhow::Result<bool> {
        let Some(raw_value) = args.get(field) else {
            return Ok(default);
        };
        if raw_value.is_null() {
            return Ok(default);
        }
        raw_value
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a boolean"))
    }

    async fn execute_action(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action") {
            None | Some(Value::Null) => "preview".to_string(),
            Some(raw_value) => match raw_value.as_str() {
                Some(raw_action) => raw_action.trim().to_ascii_lowercase(),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Invalid action type: expected string".to_string()),
                    });
                }
            },
        };

        let dry_run = match action.as_str() {
            "preview" => true,
            "migrate" => false,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Invalid action. Use 'preview' or 'migrate'.".to_string()),
                });
            }
        };

        if !dry_run {
            if let Some(blocked) = self.require_write_access() {
                return Ok(blocked);
            }
        }

        let options = OpenClawMigrationOptions {
            source_workspace: Self::parse_optional_path(args, "source_workspace")?,
            source_config: Self::parse_optional_path(args, "source_config")?,
            include_memory: Self::parse_bool(args, "include_memory", true)?,
            include_config: Self::parse_bool(args, "include_config", true)?,
            dry_run,
        };

        let report = migrate_openclaw(self.config.as_ref(), options).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "action": action,
                "merge_mode": "preserve_existing",
                "report": report,
            }))?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for OpenClawMigrationTool {
    fn name(&self) -> &str {
        "openclaw_migration"
    }

    fn description(&self) -> &str {
        "Preview or execute merge-first migration from OpenClaw (memory + config + agents) without overwriting existing ZeroClaw data."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["preview", "migrate"],
                    "description": "preview runs a dry-run report; migrate applies merge changes"
                },
                "source_workspace": {
                    "type": "string",
                    "description": "Optional OpenClaw workspace path (default ~/.openclaw/workspace)"
                },
                "source_config": {
                    "type": "string",
                    "description": "Optional OpenClaw config path (default ~/.openclaw/openclaw.json)"
                },
                "include_memory": {
                    "type": "boolean",
                    "description": "Whether to migrate memory entries (default true)"
                },
                "include_config": {
                    "type": "boolean",
                    "description": "Whether to migrate provider/channels/agents config (default true)"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        match self.execute_action(&args).await {
            Ok(result) => Ok(result),
            Err(error) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, SqliteMemory};
    use rusqlite::params;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            memory: crate::config::MemoryConfig {
                backend: "sqlite".to_string(),
                ..crate::config::MemoryConfig::default()
            },
            ..Config::default()
        }
    }

    fn seed_openclaw_workspace(source_workspace: &std::path::Path) {
        let source_db_dir = source_workspace.join("memory");
        std::fs::create_dir_all(&source_db_dir).unwrap();
        let source_db = source_db_dir.join("brain.db");
        let conn = rusqlite::Connection::open(&source_db).unwrap();
        conn.execute_batch("CREATE TABLE memories (key TEXT, content TEXT, category TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (key, content, category) VALUES (?1, ?2, ?3)",
            params!["openclaw_key", "openclaw_value", "core"],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn preview_returns_dry_run_report() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        seed_openclaw_workspace(source.path());

        let config = test_config(&target);
        let tool =
            OpenClawMigrationTool::new(Arc::new(config), Arc::new(SecurityPolicy::default()));

        let result = tool
            .execute(json!({
                "action": "preview",
                "source_workspace": source.path().display().to_string(),
                "include_config": false
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"dry_run\": true"));
        assert!(result.output.contains("\"candidates\": 1"));
    }

    #[tokio::test]
    async fn migrate_imports_memory_when_requested() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        seed_openclaw_workspace(source.path());

        let config = test_config(&target);
        let tool = OpenClawMigrationTool::new(
            Arc::new(config.clone()),
            Arc::new(SecurityPolicy::default()),
        );

        let result = tool
            .execute(json!({
                "action": "migrate",
                "source_workspace": source.path().display().to_string(),
                "include_config": false
            }))
            .await
            .unwrap();

        assert!(result.success);

        let target_memory = SqliteMemory::new(&config.workspace_dir).unwrap();
        let entry = target_memory.get("openclaw_key").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(
            entry.unwrap().category,
            MemoryCategory::Core,
            "migrated category should be preserved"
        );
    }

    #[tokio::test]
    async fn preview_rejects_when_all_modules_disabled() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        seed_openclaw_workspace(source.path());

        let config = test_config(&target);
        let tool =
            OpenClawMigrationTool::new(Arc::new(config), Arc::new(SecurityPolicy::default()));

        let result = tool
            .execute(json!({
                "action": "preview",
                "source_workspace": source.path().display().to_string(),
                "include_memory": false,
                "include_config": false
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "should fail when no migration module is enabled"
        );
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("Nothing to migrate"),
            "unexpected error message: {error}"
        );
    }

    #[tokio::test]
    async fn action_must_be_string_when_present() {
        let target = TempDir::new().unwrap();
        let config = test_config(&target);
        let tool =
            OpenClawMigrationTool::new(Arc::new(config), Arc::new(SecurityPolicy::default()));

        let result = tool.execute(json!({ "action": 123 })).await.unwrap();
        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("Invalid action type: expected string")
        );
    }

    #[tokio::test]
    async fn null_boolean_fields_use_defaults() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        seed_openclaw_workspace(source.path());

        let config = test_config(&target);
        let tool =
            OpenClawMigrationTool::new(Arc::new(config), Arc::new(SecurityPolicy::default()));

        let result = tool
            .execute(json!({
                "action": "preview",
                "source_workspace": source.path().display().to_string(),
                "include_memory": null,
                "include_config": null
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"dry_run\": true"));
    }
}
