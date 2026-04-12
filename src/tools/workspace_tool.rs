//! Tool for managing multi-client workspaces.
//!
//! Provides `workspace` subcommands: list, switch, create, info, export.

use super::traits::{Tool, ToolResult};
use crate::config::workspace::WorkspaceManager;
use crate::require_str;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Agent-callable tool for workspace management operations.
pub struct WorkspaceTool {
    manager: Arc<RwLock<WorkspaceManager>>,
    security: Arc<SecurityPolicy>,
}

impl WorkspaceTool {
    pub fn new(manager: Arc<RwLock<WorkspaceManager>>, security: Arc<SecurityPolicy>) -> Self {
        Self { manager, security }
    }
}

#[async_trait]
impl Tool for WorkspaceTool {
    fn name(&self) -> &str {
        "workspace"
    }

    fn description(&self) -> &str {
        "Manage multi-client workspaces. Subcommands: list, switch, create, info, export. Each workspace provides isolated memory, audit, secrets, and tool restrictions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "switch", "create", "info", "export"],
                    "description": "Workspace action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Workspace name (required for switch, create, export)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = require_str!(args, "action");

        let name = args.get("name").and_then(|v| v.as_str());

        match action {
            "list" => {
                let mgr = self.manager.read().await;
                let names = mgr.list();
                let active = mgr.active_name();

                if names.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No workspaces configured.".to_string(),
                        error: None,
                    });
                }

                let mut output = format!("Workspaces ({}):\n", names.len());
                for ws_name in &names {
                    let marker = if Some(*ws_name) == active {
                        " (active)"
                    } else {
                        ""
                    };
                    let _ = writeln!(output, "  - {ws_name}{marker}");
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }

            "switch" => {
                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "workspace")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                let Some(ws_name) = name else {
                    return ToolResult::err("Missing required parameter 'name' for switch action");
                };

                let mut mgr = self.manager.write().await;
                match mgr.switch(ws_name) {
                    Ok(profile) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Switched to workspace '{}'. Memory namespace: {}, Audit namespace: {}",
                            profile.name,
                            profile.effective_memory_namespace(),
                            profile.effective_audit_namespace()
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }),
                }
            }

            "create" => {
                if let Err(error) = self
                    .security
                    .enforce_tool_operation(ToolOperation::Act, "workspace")
                {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(error),
                    });
                }

                let Some(ws_name) = name else {
                    return ToolResult::err("Missing required parameter 'name' for create action");
                };

                let mut mgr = self.manager.write().await;
                match mgr.create(ws_name).await {
                    Ok(profile) => {
                        let name = profile.name.clone();
                        let dir = mgr.workspace_dir(ws_name);
                        Ok(ToolResult {
                            success: true,
                            output: format!("Created workspace '{}' at {}", name, dir.display()),
                            error: None,
                        })
                    }
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }),
                }
            }

            "info" => {
                let mgr = self.manager.read().await;
                let target_name = name.or_else(|| mgr.active_name());

                match target_name {
                    Some(ws_name) => match mgr.get(ws_name) {
                        Some(profile) => {
                            let is_active = mgr.active_name() == Some(ws_name);
                            let mut output = format!("Workspace: {}\n", profile.name);
                            let _ = writeln!(
                                output,
                                "  Status: {}",
                                if is_active { "active" } else { "inactive" }
                            );
                            let _ = writeln!(
                                output,
                                "  Memory namespace: {}",
                                profile.effective_memory_namespace()
                            );
                            let _ = writeln!(
                                output,
                                "  Audit namespace: {}",
                                profile.effective_audit_namespace()
                            );
                            if !profile.allowed_domains.is_empty() {
                                let _ = writeln!(
                                    output,
                                    "  Allowed domains: {}",
                                    profile.allowed_domains.join(", ")
                                );
                            }
                            if !profile.tool_restrictions.is_empty() {
                                let _ = writeln!(
                                    output,
                                    "  Restricted tools: {}",
                                    profile.tool_restrictions.join(", ")
                                );
                            }
                            Ok(ToolResult {
                                success: true,
                                output,
                                error: None,
                            })
                        }
                        None => Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("workspace '{}' not found", ws_name)),
                        }),
                    },
                    None => Ok(ToolResult {
                        success: true,
                        output: "No workspace is currently active. Use 'workspace switch <name>' to activate one.".to_string(),
                        error: None,
                    }),
                }
            }

            "export" => {
                let mgr = self.manager.read().await;
                let ws_name = name.or_else(|| mgr.active_name()).ok_or_else(|| {
                    anyhow::anyhow!("'name' parameter is required when no workspace is active")
                })?;

                match mgr.export(ws_name) {
                    Ok(toml_str) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Exported workspace '{}' config (secrets redacted):\n\n{}",
                            ws_name, toml_str
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }),
                }
            }

            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "unknown workspace action '{}'. Expected: list, switch, create, info, export",
                    other
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use tempfile::TempDir;

    fn test_tool(tmp: &TempDir) -> WorkspaceTool {
        let mgr = WorkspaceManager::new(tmp.path().to_path_buf());
        WorkspaceTool::new(
            Arc::new(RwLock::new(mgr)),
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[tokio::test]
    async fn workspace_tool_list_empty() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(&tmp);
        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No workspaces"));
    }

    #[tokio::test]
    async fn workspace_tool_create_and_list() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(&tmp);

        let result = tool
            .execute(json!({"action": "create", "name": "test_client"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("test_client"));

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("test_client"));
    }

    #[tokio::test]
    async fn workspace_tool_switch_and_info() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(&tmp);

        tool.execute(json!({"action": "create", "name": "ws_test"}))
            .await
            .unwrap();

        let result = tool
            .execute(json!({"action": "switch", "name": "ws_test"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Switched to workspace"));

        let result = tool.execute(json!({"action": "info"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("ws_test"));
        assert!(result.output.contains("active"));
    }

    #[tokio::test]
    async fn workspace_tool_export_redacts() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(&tmp);

        tool.execute(json!({"action": "create", "name": "export_ws"}))
            .await
            .unwrap();

        let result = tool
            .execute(json!({"action": "export", "name": "export_ws"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("export_ws"));
    }

    #[tokio::test]
    async fn workspace_tool_unknown_action() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(&tmp);
        let result = tool.execute(json!({"action": "destroy"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("unknown workspace action"));
    }

    #[tokio::test]
    async fn workspace_tool_switch_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(&tmp);
        let result = tool
            .execute(json!({"action": "switch", "name": "ghost"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));
    }
}
