use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::fs;

/// Advanced file and directory removal tool with safety guards.
pub struct FileRemoveTool {
    security: Arc<SecurityPolicy>,
}

impl FileRemoveTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for FileRemoveTool {
    fn name(&self) -> &str {
        "file_remove"
    }

    fn description(&self) -> &str {
        "Remove a file or directory. Supports recoverable deletion via trash by default."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file or directory to remove."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "If true, remove directory and all its contents recursively. Default: false.",
                    "default": false
                },
                "permanent": {
                    "type": "boolean",
                    "description": "If true, bypass trash and delete permanently. USE WITH CAUTION. Default: false.",
                    "default": false
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let recursive = args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);
        let permanent = args.get("permanent").and_then(|v| v.as_bool()).unwrap_or(false);

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // Security check: validate path is within allowed boundaries
        if !self.security.is_path_allowed(path_str) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path_str}")),
            });
        }

        let full_path = self.security.workspace_dir.join(path_str);

        // Check if path exists
        if !full_path.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path does not exist: {path_str}")),
            });
        }

        let metadata = fs::metadata(&full_path).await?;
        let is_dir = metadata.is_dir();

        if is_dir && !recursive {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("'{}' is a directory. Set 'recursive: true' to remove it.", path_str)),
            });
        }

        if permanent {
            // Permanent deletion
            if is_dir {
                fs::remove_dir_all(&full_path).await?;
            } else {
                fs::remove_file(&full_path).await?;
            }
            Ok(ToolResult {
                success: true,
                output: format!("Permanently removed {}: {}", if is_dir { "directory" } else { "file" }, path_str),
                error: None,
            })
        } else {
            // Attempt to move to trash
            match trash::delete(&full_path) {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    output: format!("Moved {} to trash: {}", if is_dir { "directory" } else { "file" }, path_str),
                    error: None,
                }),
                Err(e) => {
                    tracing::warn!("Failed to use trash system: {}. Falling back to permanent deletion check.", e);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Trash not supported or failed: {}. Use 'permanent: true' if you want to force deletion.", e)),
                    })
                }
            }
        }
    }
}
