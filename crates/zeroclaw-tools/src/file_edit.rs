use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Edit a file by replacing an exact string match with new content.
///
/// Uses `old_string` → `new_string` precise replacement within the workspace.
/// The `old_string` must appear exactly once in the file (zero matches = not
/// found, multiple matches = ambiguous). `new_string` may be empty to delete
/// the matched text.
pub struct FileEditTool {
    security: Arc<SecurityPolicy>,
}

impl FileEditTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    /// Validate and resolve a caller-supplied path to an absolute candidate.
    /// See [`super::file_write::FileWriteTool::resolve_candidate`] for details.
    fn resolve_candidate(&self, path: &str) -> anyhow::Result<std::path::PathBuf> {
        let workspace_dir = &self.security.workspace_dir;

        if path.contains('\0') {
            anyhow::bail!("Path not allowed: contains null byte");
        }
        if std::path::Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!("Path not allowed by security policy: {path}");
        }

        let p = std::path::Path::new(path);
        if p.is_absolute() {
            let workspace_canonical = workspace_dir
                .canonicalize()
                .unwrap_or_else(|_| workspace_dir.clone());
            if !p.starts_with(&workspace_canonical) && !p.starts_with(workspace_dir.as_path()) {
                anyhow::bail!("Path not allowed by security policy: {path}");
            }
            return Ok(p.to_path_buf());
        }

        if let Ok(workspace_rootless) = workspace_dir.strip_prefix("/") {
            if let Ok(stripped) = p.strip_prefix(workspace_rootless) {
                return Ok(if stripped.as_os_str().is_empty() {
                    workspace_dir.clone()
                } else {
                    workspace_dir.join(stripped)
                });
            }
        }

        Ok(workspace_dir.join(p))
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string match with new content"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths resolve from workspace root; absolute paths must be within the workspace."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find and replace (must appear exactly once in the file)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text (empty string to delete the matched text)"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // ── 1. Extract parameters ──────────────────────────────────
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'old_string' parameter"))?;

        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'new_string' parameter"))?;

        if old_string.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("old_string must not be empty".into()),
            });
        }

        // ── 2. Autonomy check ──────────────────────────────────────
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // ── 3. Rate limit check ────────────────────────────────────
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // ── 4. Resolve candidate path ──────────────────────────────
        let full_path = match self.resolve_candidate(path) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        // ── 5. Canonicalise parent ─────────────────────────────────
        let Some(parent) = full_path.parent() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid path: missing parent directory".into()),
            });
        };

        let resolved_parent = match tokio::fs::canonicalize(parent).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        // ── 6. Workspace boundary check ────────────────────────────
        let workspace_canonical = self
            .security
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.security.workspace_dir.clone());

        if !resolved_parent.starts_with(&workspace_canonical) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path escapes workspace directory: {path}")),
            });
        }

        let Some(file_name) = full_path.file_name() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid path: missing file name".into()),
            });
        };

        let resolved_target = resolved_parent.join(file_name);

        // ── 7. Symlink check ───────────────────────────────────────
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await
            && meta.file_type().is_symlink()
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Refusing to edit through symlink: {}",
                    resolved_target.display()
                )),
            });
        }

        // ── 8. Record action ───────────────────────────────────────
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // ── 9. Read → match → replace → write ─────────────────────
        let content = match tokio::fs::read_to_string(&resolved_target).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file: {e}")),
                });
            }
        };

        let match_count = content.matches(old_string).count();

        if match_count == 0 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("old_string not found in file".into()),
            });
        }

        if match_count > 1 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "old_string matches {match_count} times; must match exactly once"
                )),
            });
        }

        let new_content = content.replacen(old_string, new_string, 1);

        match tokio::fs::write(&resolved_target, &new_content).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Edited {path}: replaced 1 occurrence ({} bytes)",
                    new_content.len()
                ),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write file: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool(workspace: std::path::PathBuf) -> FileEditTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        FileEditTool::new(security)
    }

    fn test_tool_with(
        workspace: std::path::PathBuf,
        autonomy: AutonomyLevel,
        max_actions_per_hour: u32,
    ) -> FileEditTool {
        let security = Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: workspace,
            max_actions_per_hour,
            ..SecurityPolicy::default()
        });
        FileEditTool::new(security)
    }

    #[test]
    fn file_edit_name() {
        let tool = test_tool(std::env::temp_dir());
        assert_eq!(tool.name(), "file_edit");
    }

    #[test]
    fn file_edit_schema_has_required_params() {
        let tool = test_tool(std::env::temp_dir());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["old_string"].is_object());
        assert!(schema["properties"]["new_string"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("old_string")));
        assert!(required.contains(&json!("new_string")));
    }

    #[tokio::test]
    async fn file_edit_replaces_single_match() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_single");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "goodbye"
            }))
            .await
            .unwrap();

        assert!(result.success, "edit should succeed: {:?}", result.error);
        assert!(result.output.contains("replaced 1 occurrence"));

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "goodbye world");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_not_found() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_notfound");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "nonexistent",
                "new_string": "replacement"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("not found"));

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello world");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_multiple_matches() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_multi");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "aaa bbb aaa")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "aaa",
                "new_string": "ccc"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("matches 2 times")
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "aaa bbb aaa");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_delete_via_empty_new_string() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_delete");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "keep remove keep")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": " remove",
                "new_string": ""
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "delete edit should succeed: {:?}",
            result.error
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "keep keep");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_missing_path_param() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool
            .execute(json!({"old_string": "a", "new_string": "b"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_missing_old_string_param() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool
            .execute(json!({"path": "f.txt", "new_string": "b"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_missing_new_string_param() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool
            .execute(json!({"path": "f.txt", "old_string": "a"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_rejects_empty_old_string() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_empty_old_string");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "",
                "new_string": "x"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("must not be empty")
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_path_traversal() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_traversal");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "../../etc/passwd",
                "old_string": "root",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_absolute_path() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool
            .execute(json!({
                "path": "/etc/passwd",
                "old_string": "root",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_edit_normalizes_workspace_prefixed_relative_path() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_edit_workspace_prefixed");
        let workspace = root.join("workspace");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(workspace.join("nested"))
            .await
            .unwrap();
        tokio::fs::write(workspace.join("nested/target.txt"), "hello world")
            .await
            .unwrap();

        let tool = test_tool(workspace.clone());
        let workspace_prefixed = workspace
            .strip_prefix(std::path::Path::new("/"))
            .unwrap()
            .join("nested/target.txt");
        let result = tool
            .execute(json!({
                "path": workspace_prefixed.to_string_lossy(),
                "old_string": "world",
                "new_string": "zeroclaw"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let content = tokio::fs::read_to_string(workspace.join("nested/target.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello zeroclaw");
        assert!(!workspace.join(workspace_prefixed).exists());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_edit_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_file_edit_symlink_escape");
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        symlink(&outside, workspace.join("escape_dir")).unwrap();

        let tool = test_tool(workspace.clone());
        let result = tool
            .execute(json!({
                "path": "escape_dir/target.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("escapes workspace")
        );

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_edit_blocks_symlink_target_file() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_file_edit_symlink_target");
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        tokio::fs::write(outside.join("target.txt"), "original")
            .await
            .unwrap();
        symlink(outside.join("target.txt"), workspace.join("linked.txt")).unwrap();

        let tool = test_tool(workspace.clone());
        let result = tool
            .execute(json!({
                "path": "linked.txt",
                "old_string": "original",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success, "editing through symlink must be blocked");
        assert!(
            result.error.as_deref().unwrap_or("").contains("symlink"),
            "error should mention symlink"
        );

        let content = tokio::fs::read_to_string(outside.join("target.txt"))
            .await
            .unwrap();
        assert_eq!(content, "original", "original file must not be modified");

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_readonly_mode() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_readonly");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let tool = test_tool_with(dir.clone(), AutonomyLevel::ReadOnly, 20);
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "world"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("read-only"));

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_when_rate_limited() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_rate_limited");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let tool = test_tool_with(dir.clone(), AutonomyLevel::Supervised, 0);
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "world"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Rate limit exceeded")
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_nonexistent_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_nofile");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "missing.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to read file")
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_absolute_path_in_workspace() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_abs_path");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Canonicalize so the workspace dir matches resolved paths on macOS (/private/var/…)
        let dir = tokio::fs::canonicalize(&dir).await.unwrap();

        tokio::fs::write(dir.join("target.txt"), "old content")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());

        let abs_path = dir.join("target.txt");
        let result = tool
            .execute(json!({
                "path": abs_path.to_string_lossy().to_string(),
                "old_string": "old content",
                "new_string": "new content"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "editing via absolute workspace path should succeed, error: {:?}",
            result.error
        );

        let content = tokio::fs::read_to_string(dir.join("target.txt"))
            .await
            .unwrap();
        assert_eq!(content, "new content");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_null_byte_in_path() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_null_byte");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({
                "path": "test\0evil.txt",
                "old_string": "old",
                "new_string": "new"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_path_outside_workspace() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_edit_outside_workspace");
        let workspace = root.join("workspace");
        let outside = root.join("outside.txt");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(&outside, "original").await.unwrap();

        let tool = test_tool(workspace.clone());
        let result = tool
            .execute(json!({
                "path": outside.to_string_lossy(),
                "old_string": "original",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let content = tokio::fs::read_to_string(&outside).await.unwrap();
        assert_eq!(
            content, "original",
            "file outside workspace must not be modified"
        );

        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
