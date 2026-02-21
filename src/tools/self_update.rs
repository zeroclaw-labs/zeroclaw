use super::traits::{Tool, ToolResult};
use crate::updater::{self, UpdateApplyOptions};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

/// Tool for checking/applying ZeroClaw self-updates.
pub struct SelfUpdateTool;

impl SelfUpdateTool {
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SelfUpdateTool {
    fn name(&self) -> &str {
        "self_update"
    }

    fn description(&self) -> &str {
        "Check for new ZeroClaw releases or apply a binary self-update. Apply requires approved=true unless dry_run=true."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["check", "apply"],
                    "description": "check = inspect release status, apply = download/extract/install"
                },
                "version": {
                    "type": "string",
                    "description": "Optional target version (for example: 0.1.0 or v0.1.0). Defaults to latest release"
                },
                "install_path": {
                    "type": "string",
                    "description": "Optional binary install path override"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview apply without changing files",
                    "default": false
                },
                "approved": {
                    "type": "boolean",
                    "description": "Explicit operator confirmation required for apply actions",
                    "default": false
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("check");
        let version = args
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let install_path = args
            .get("install_path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from);
        let dry_run = args
            .get("dry_run")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        match action {
            "check" => {
                let check =
                    updater::check_for_updates(env!("CARGO_PKG_VERSION"), version.as_deref())
                        .await?;
                let payload = json!({
                    "current_version": check.current_version,
                    "latest_version": check.latest_version,
                    "update_available": check.update_available,
                    "release": {
                        "tag": check.release.tag_name,
                        "url": check.release.html_url,
                        "published_at": check.release.published_at,
                    }
                });

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&payload)?,
                    error: None,
                })
            }
            "apply" => {
                let approved = args
                    .get("approved")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if !approved && !dry_run {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "self_update apply requires explicit approval (approved=true), or run with dry_run=true".to_string(),
                        ),
                    });
                }

                let result = updater::apply_update(UpdateApplyOptions {
                    target_version: version,
                    install_path,
                    dry_run,
                })
                .await?;
                let payload = json!({
                    "from_version": result.from_version,
                    "to_version": result.to_version,
                    "target": result.target,
                    "asset_name": result.asset_name,
                    "install_path": result.install_path,
                    "dry_run": result.dry_run,
                    "release_url": result.release_url,
                });

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&payload)?,
                    error: None,
                })
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported action '{other}'. Use 'check' or 'apply'"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_is_stable() {
        let tool = SelfUpdateTool::new();
        assert_eq!(tool.name(), "self_update");
    }

    #[test]
    fn schema_exposes_approval_and_actions() {
        let tool = SelfUpdateTool::new();
        let schema = tool.parameters_schema();

        assert_eq!(
            schema["properties"]["action"]["enum"],
            json!(["check", "apply"])
        );
        assert!(schema["properties"]["approved"].is_object());
        assert!(schema["properties"]["dry_run"].is_object());
    }

    #[tokio::test]
    async fn apply_requires_approval_unless_dry_run() {
        let tool = SelfUpdateTool::new();
        let result = tool.execute(json!({"action": "apply"})).await.unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("requires explicit approval"));
    }

    #[tokio::test]
    async fn unsupported_action_returns_failure() {
        let tool = SelfUpdateTool::new();
        let result = tool.execute(json!({"action": "explode"})).await.unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Unsupported action"));
    }
}
