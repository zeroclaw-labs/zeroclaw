use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

const MAX_RESULTS: usize = 1000;

/// Search for files by glob pattern within the workspace.
pub struct GlobSearchTool {
    security: Arc<SecurityPolicy>,
}

impl GlobSearchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for GlobSearchTool {
    fn name(&self) -> &str {
        "glob_search"
    }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern within the workspace. \
         Returns a sorted list of matching file paths relative to the workspace root when possible, \
         or canonical absolute paths for matches under allowed_roots. \
         Examples: '**/*.rs' (all Rust files), 'src/**/mod.rs' (all mod.rs in src)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files, e.g. '**/*.rs', 'src/**/mod.rs'"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' parameter"))?;

        // Rate limit check (fast path)
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Security: reject path traversal
        if pattern.contains("../") || pattern.contains("..\\") || pattern == ".." {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Path traversal ('..') is not allowed in glob patterns.".into()),
            });
        }

        if !self.security.is_path_allowed(pattern) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {pattern}")),
            });
        }

        // Record action to consume rate limit budget
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Build full pattern anchored to workspace unless the user supplied
        // an absolute / `~/...` pattern that is permitted by policy.
        let workspace = &self.security.workspace_dir;
        let full_pattern = {
            let resolved = self.security.resolve_user_path(pattern);
            if std::path::Path::new(pattern).is_absolute()
                || pattern == "~"
                || pattern.starts_with("~/")
            {
                resolved.to_string_lossy().to_string()
            } else {
                let workspace_prefix = glob::Pattern::escape(workspace.to_string_lossy().as_ref());
                if workspace_prefix.ends_with(std::path::MAIN_SEPARATOR) {
                    format!("{workspace_prefix}{pattern}")
                } else {
                    format!("{workspace_prefix}{}{pattern}", std::path::MAIN_SEPARATOR)
                }
            }
        };

        let entries = match glob::glob(&full_pattern) {
            Ok(paths) => paths,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Invalid glob pattern: {e}")),
                });
            }
        };

        let workspace_canon = match std::fs::canonicalize(workspace) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot resolve workspace directory: {e}")),
                });
            }
        };

        let mut results = Vec::new();
        let mut truncated = false;

        for entry in entries {
            let path = match entry {
                Ok(p) => p,
                Err(_) => continue, // skip unreadable entries
            };

            // Canonicalize to resolve symlinks, then verify still inside workspace
            let resolved = match std::fs::canonicalize(&path) {
                Ok(p) => p,
                Err(_) => continue, // skip broken symlinks / unresolvable paths
            };

            if !self.security.is_resolved_path_allowed(&resolved) {
                continue; // silently filter symlink escapes
            }

            // Only include files, not directories
            if resolved.is_dir() {
                continue;
            }

            // Convert to workspace-relative path when possible, otherwise keep
            // the canonical absolute path (e.g. results from allowed_roots).
            if let Ok(rel) = resolved.strip_prefix(&workspace_canon) {
                results.push(rel.to_string_lossy().to_string());
            } else {
                results.push(resolved.to_string_lossy().to_string());
            }

            if results.len() >= MAX_RESULTS {
                truncated = true;
                break;
            }
        }

        results.sort();

        let output = if results.is_empty() {
            format!("No files matching pattern '{pattern}' found in workspace or allowed roots.")
        } else {
            use std::fmt::Write;
            let mut buf = results.join("\n");
            if truncated {
                let _ = write!(
                    buf,
                    "\n\n[Results truncated: showing first {MAX_RESULTS} of more matches]"
                );
            }
            let _ = write!(buf, "\n\nTotal: {} files", results.len());
            buf
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_security(workspace: PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with(
        workspace: PathBuf,
        autonomy: AutonomyLevel,
        max_actions_per_hour: u32,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: workspace,
            max_actions_per_hour,
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn glob_search_name_and_schema() {
        let tool = GlobSearchTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "glob_search");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["pattern"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("pattern")));
    }

    #[tokio::test]
    async fn glob_search_single_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "content").unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "hello.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello.txt"));
    }

    #[tokio::test]
    async fn glob_search_multiple_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("c.rs"), "").unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "*.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("a.txt"));
        assert!(result.output.contains("b.txt"));
        assert!(!result.output.contains("c.rs"));
    }

    #[tokio::test]
    async fn glob_search_recursive() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("root.txt"), "").unwrap();
        std::fs::write(dir.path().join("sub/mid.txt"), "").unwrap();
        std::fs::write(dir.path().join("sub/deep/leaf.txt"), "").unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "**/*.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("root.txt"));
        assert!(result.output.contains("mid.txt"));
        assert!(result.output.contains("leaf.txt"));
    }

    #[tokio::test]
    async fn glob_search_no_matches() {
        let dir = TempDir::new().unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "*.nonexistent"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("No files matching pattern"));
    }

    #[tokio::test]
    async fn glob_search_missing_param() {
        let tool = GlobSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn glob_search_rejects_absolute_path_outside_allowed_roots() {
        let tool = GlobSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"pattern": "/etc/**/*"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn glob_search_allows_absolute_pattern_inside_allowed_roots() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        let outside_match = outside.join("a.md");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(&outside_match, "alpha").unwrap();
        std::fs::write(outside.join("b.txt"), "beta").unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            workspace_only: true,
            allowed_roots: vec![outside.clone()],
            forbidden_paths: vec![],
            ..SecurityPolicy::default()
        });
        let tool = GlobSearchTool::new(security);
        let pattern = outside.join("*.md").to_string_lossy().to_string();
        let result = tool.execute(json!({"pattern": pattern})).await.unwrap();
        let outside_match = std::fs::canonicalize(outside_match).unwrap();

        assert!(result.success);
        assert!(result
            .output
            .contains(outside_match.to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn glob_search_escapes_workspace_prefix() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace[glob]");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("hello.txt"), "content").unwrap();

        let tool = GlobSearchTool::new(test_security(workspace));
        let result = tool.execute(json!({"pattern": "*.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello.txt"));
    }

    #[tokio::test]
    async fn glob_search_allows_tilde_pattern_inside_allowed_roots() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            return;
        };

        let root = home.join("zeroclaw_test_glob_search_allowed_roots_tilde");
        let workspace = root.join("workspace");
        let outside = root.join("projects_root");
        let outside_match = outside.join("notes.md");

        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(&outside_match, "tilde-allowed").unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            workspace_only: true,
            allowed_roots: vec![outside.clone()],
            forbidden_paths: vec![],
            ..SecurityPolicy::default()
        });
        let tool = GlobSearchTool::new(security);
        let result = tool
            .execute(json!({"pattern": "~/zeroclaw_test_glob_search_allowed_roots_tilde/projects_root/*.md"}))
            .await
            .unwrap();
        let outside_match = std::fs::canonicalize(outside_match).unwrap();

        assert!(result.success);
        assert!(result
            .output
            .contains(outside_match.to_string_lossy().as_ref()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn glob_search_rejects_path_traversal() {
        let tool = GlobSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"pattern": "../../../etc/passwd"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Path traversal"));
    }

    #[tokio::test]
    async fn glob_search_rejects_dotdot_only() {
        let tool = GlobSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"pattern": ".."})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Path traversal"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn glob_search_filters_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");

        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "leaked").unwrap();

        // Symlink inside workspace pointing outside
        symlink(outside.join("secret.txt"), workspace.join("escape.txt")).unwrap();
        // Also add a legitimate file
        std::fs::write(workspace.join("legit.txt"), "ok").unwrap();

        let tool = GlobSearchTool::new(test_security(workspace.clone()));
        let result = tool.execute(json!({"pattern": "*.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("legit.txt"));
        assert!(!result.output.contains("escape.txt"));
        assert!(!result.output.contains("secret.txt"));
    }

    #[tokio::test]
    async fn glob_search_readonly_mode() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = GlobSearchTool::new(test_security_with(
            dir.path().to_path_buf(),
            AutonomyLevel::ReadOnly,
            20,
        ));
        let result = tool.execute(json!({"pattern": "*.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("file.txt"));
    }

    #[tokio::test]
    async fn glob_search_rate_limited() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = GlobSearchTool::new(test_security_with(
            dir.path().to_path_buf(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool.execute(json!({"pattern": "*.txt"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Rate limit"));
    }

    #[tokio::test]
    async fn glob_search_results_sorted() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "*.txt"})).await.unwrap();

        assert!(result.success);
        let lines: Vec<&str> = result.output.lines().collect();
        // First 3 lines should be the sorted file names
        assert!(lines.len() >= 3);
        assert_eq!(lines[0], "a.txt");
        assert_eq!(lines[1], "b.txt");
        assert_eq!(lines[2], "c.txt");
    }

    #[tokio::test]
    async fn glob_search_excludes_directories() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "").unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "*"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("file.txt"));
        assert!(!result.output.contains("subdir"));
    }

    #[tokio::test]
    async fn glob_search_invalid_pattern() {
        let dir = TempDir::new().unwrap();

        let tool = GlobSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "[invalid"})).await.unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_ref()
            .unwrap()
            .contains("Invalid glob pattern"));
    }
}
