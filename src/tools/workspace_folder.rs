//! Tool that lets the user grant the agent access to a specific folder
//! on their computer via a chat message (e.g. "~/Documents 폴더에서 작업해줘").
//!
//! When executed, it validates the path, adds it to the runtime `allowed_roots`
//! in [`SecurityPolicy`], and returns the folder listing so the agent can
//! immediately start working with the files.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tool for granting runtime access to a user-specified folder.
pub struct WorkspaceFolderTool {
    security: Arc<SecurityPolicy>,
}

impl WorkspaceFolderTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    /// Expand `~` to the user's home directory.
    fn expand_path(raw: &str) -> PathBuf {
        let trimmed = raw.trim();
        if trimmed.starts_with("~/") || trimmed == "~" {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(trimmed.strip_prefix("~/").unwrap_or(""));
            }
        }
        PathBuf::from(trimmed)
    }

    /// List top-level contents of a directory (files + subdirs), up to `limit`.
    async fn list_directory(path: &Path, limit: usize) -> anyhow::Result<Vec<String>> {
        let mut entries = Vec::new();
        let mut reader = tokio::fs::read_dir(path).await?;
        while let Some(entry) = reader.next_entry().await? {
            if entries.len() >= limit {
                entries.push("... (more files)".into());
                break;
            }
            let file_type = entry.file_type().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                entries.push(format!("{}/", name));
            } else {
                entries.push(name);
            }
        }
        entries.sort();
        Ok(entries)
    }
}

#[async_trait]
impl Tool for WorkspaceFolderTool {
    fn name(&self) -> &str {
        "workspace_folder"
    }

    fn description(&self) -> &str {
        "Grant the agent access to a folder on the user's computer. \
         After calling this tool, the agent can read, write, edit, and search \
         all files inside the folder using file_read, file_write, file_edit, \
         glob_search, content_search, and document_process tools. \
         Use this when the user says something like '~/Documents 폴더에서 작업해줘' \
         or specifies a folder path for file operations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path or ~/relative path to the folder (e.g. '/home/user/Documents' or '~/Documents')"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let raw_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // Expand and validate path
        let expanded = Self::expand_path(raw_path);

        if !expanded.is_absolute() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Path must be absolute or start with ~/. Got: {raw_path}"
                )),
            });
        }

        if !expanded.is_dir() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Directory does not exist: {}", expanded.display())),
            });
        }

        // Block obviously sensitive directories
        let sensitive = [
            "/.ssh",
            "/.gnupg",
            "/.aws",
            "/.config/gcloud",
            "/etc",
            "/root",
            "/usr",
            "/bin",
            "/sbin",
            "/boot",
            "/dev",
            "/proc",
            "/sys",
        ];
        let path_str = expanded.to_string_lossy();
        for s in &sensitive {
            if path_str.contains(s) || path_str.ends_with(s.trim_start_matches('/')) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Cannot grant access to sensitive directory: {}",
                        expanded.display()
                    )),
                });
            }
        }

        // Add to allowed_roots
        let added = self.security.add_allowed_root(&expanded);
        let status = if added {
            "Folder access granted"
        } else {
            "Folder already accessible"
        };

        // List directory contents
        let entries = match Self::list_directory(&expanded, 50).await {
            Ok(e) => e,
            Err(e) => vec![format!("(could not list: {e})")],
        };

        let output = json!({
            "status": status,
            "folder": expanded.to_string_lossy(),
            "file_count": entries.len(),
            "contents": entries,
            "instructions": "You can now use file_read, file_write, file_edit, glob_search, content_search, and document_process on files inside this folder. Use absolute paths."
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output).unwrap_or_default(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn name_and_description() {
        let tool = WorkspaceFolderTool::new(test_security());
        assert_eq!(tool.name(), "workspace_folder");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn expand_tilde_path() {
        let expanded = WorkspaceFolderTool::expand_path("~/Documents");
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expanded, PathBuf::from(home).join("Documents"));
        }
    }

    #[tokio::test]
    async fn rejects_relative_path() {
        let tool = WorkspaceFolderTool::new(test_security());
        let result = tool
            .execute(json!({"path": "relative/path"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("absolute"));
    }

    #[tokio::test]
    async fn rejects_sensitive_directory() {
        let tool = WorkspaceFolderTool::new(test_security());
        let result = tool.execute(json!({"path": "/root/.ssh"})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        // On macOS /root/.ssh doesn't exist → "does not exist" error
        // On Linux /root/.ssh may exist → "sensitive" error
        // Both outcomes correctly reject the path.
        assert!(
            err.contains("sensitive") || err.contains("does not exist"),
            "expected sensitive or not-exist rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_nonexistent_directory() {
        let tool = WorkspaceFolderTool::new(test_security());
        let result = tool
            .execute(json!({"path": "/nonexistent_zeroclaw_test_dir_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("does not exist"));
    }

    #[tokio::test]
    async fn grants_access_to_temp_dir() {
        let dir = std::env::temp_dir().join("zeroclaw_wf_test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("test.txt"), "hello").unwrap();

        let security = test_security();
        let tool = WorkspaceFolderTool::new(security.clone());
        let result = tool
            .execute(json!({"path": dir.to_string_lossy().as_ref()}))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(result.output.contains("Folder access granted"));

        // Verify it was added to allowed_roots
        let roots = security.allowed_roots_snapshot();
        let canonical = dir.canonicalize().unwrap();
        assert!(
            roots.contains(&canonical),
            "allowed_roots should contain the dir"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
