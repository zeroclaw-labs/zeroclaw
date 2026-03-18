use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Surgical file editing via find-and-replace.
///
/// Replaces exactly one occurrence of `old_text` with `new_text` in the target
/// file. Safer and more token-efficient than whole-file rewrites with
/// `file_write` because only the diff is transmitted.
pub struct FileEditTool {
    security: Arc<SecurityPolicy>,
}

impl FileEditTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Replace a specific text span in a file (surgical edit). Provide old_text (exact match, including whitespace) and new_text. Exactly one occurrence must match."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Relative path to the file within the workspace"
                },
                "old_text": {
                    "type": "string",
                    "description": "Exact text to find (must match exactly one location in the file, including whitespace and indentation)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Replacement text"
                }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let old_text = args
            .get("old_text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'old_text' parameter"))?;

        let new_text = args
            .get("new_text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'new_text' parameter"))?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        let full_path = self.security.workspace_dir.join(path);

        // File must exist for edit operations
        let resolved = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Resolved path escapes workspace: {}",
                    resolved.display()
                )),
            });
        }

        // Refuse symlinks
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved).await {
            if meta.file_type().is_symlink() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Refusing to edit through symlink: {}",
                        resolved.display()
                    )),
                });
            }
        }

        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file: {e}")),
                });
            }
        };

        let match_count = content.matches(old_text).count();
        if match_count == 0 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "old_text not found in file. Ensure exact match including whitespace and indentation."
                        .into(),
                ),
            });
        }
        if match_count > 1 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "old_text matches {match_count} locations. Include more surrounding context to make the match unique."
                )),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let new_content = content.replacen(old_text, new_text, 1);

        match tokio::fs::write(&resolved, &new_content).await {
            Ok(()) => {
                let old_lines = old_text.lines().count();
                let new_lines = new_text.lines().count();
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Edited {path}: replaced {old_lines} line(s) with {new_lines} line(s) ({} bytes → {} bytes)",
                        content.len(),
                        new_content.len()
                    ),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write edited file: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    #[tokio::test]
    async fn edit_replaces_exact_match() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "hello world\nfoo bar\nbaz").unwrap();

        let tool = FileEditTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(serde_json::json!({
                "path": "test.txt",
                "old_text": "foo bar",
                "new_text": "replaced"
            }))
            .await
            .unwrap();

        assert!(result.success, "edit should succeed: {:?}", result.error);
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "hello world\nreplaced\nbaz");
    }

    #[tokio::test]
    async fn edit_rejects_ambiguous_match() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("dup.txt");
        std::fs::write(&file_path, "aaa\naaa\nbbb").unwrap();

        let tool = FileEditTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(serde_json::json!({
                "path": "dup.txt",
                "old_text": "aaa",
                "new_text": "ccc"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("2 locations"));
    }

    #[tokio::test]
    async fn edit_rejects_missing_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("miss.txt"), "hello world").unwrap();

        let tool = FileEditTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(serde_json::json!({
                "path": "miss.txt",
                "old_text": "not here",
                "new_text": "replaced"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));
    }
}
