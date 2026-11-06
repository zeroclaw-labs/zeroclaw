use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

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
        "Search for files matching a glob pattern within the workspace, or under an \
         allowed root with an absolute pattern. Returns a sorted list of matching file \
         paths: relative to the workspace root, absolute for files outside it. \
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
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "pattern"})),
                    "glob_search: missing pattern parameter"
                );
                anyhow::Error::msg("Missing 'pattern' parameter")
            })?;

        // Rate limiting and path-allowlist checks are applied by the
        // RateLimitedTool + PathGuardedTool wrappers at registration time
        // (see zeroclaw-runtime::tools::mod).

        // Security: reject absolute paths unless under a readable allowed root
        // (read-write or read-only); each match is still checked for readability.
        if (pattern.starts_with('/') || pattern.starts_with('\\'))
            && !self.security.is_under_any_allowed_root(pattern)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Absolute paths are not allowed. Use a relative glob pattern.".into()),
            });
        }

        // Security: reject path traversal
        if pattern.contains("../") || pattern.contains("..\\") || pattern == ".." {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Path traversal ('..') is not allowed in glob patterns.".into()),
            });
        }

        // Build full pattern: use resolve_tool_path to handle tilde expansion
        // and absolute paths correctly.
        let full_pattern = self
            .security
            .resolve_tool_path(pattern)
            .to_string_lossy()
            .to_string();

        let entries = match glob::glob(&full_pattern) {
            Ok(paths) => paths,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Invalid glob pattern: {e}")),
                });
            }
        };

        let workspace = &self.security.workspace_dir;
        let workspace_canon = match std::fs::canonicalize(workspace) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
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

            if !self.security.is_resolved_path_readable(&resolved) {
                continue;
            }

            // Only include files, not directories
            if resolved.is_dir() {
                continue;
            }

            // Workspace files are reported relative to it; files under another
            // readable root (an absolute pattern accepted above) keep their
            // absolute path instead of being dropped.
            match resolved.strip_prefix(&workspace_canon) {
                Ok(rel) => results.push(rel.to_string_lossy().to_string()),
                Err(_) => results.push(resolved.to_string_lossy().to_string()),
            }

            if results.len() >= MAX_RESULTS {
                truncated = true;
                break;
            }
        }

        results.sort();

        let output = if results.is_empty() {
            format!("No files matching pattern '{pattern}' found in workspace.")
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
            output: output.into(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

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
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("pattern"))
        );
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
    async fn glob_search_rejects_absolute_path() {
        let tool = GlobSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"pattern": "/etc/**/*"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Absolute paths"));
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

    // Rate-limit behavior is covered by RateLimitedTool's own tests in
    // zeroclaw-tools::wrappers; this tool delegates the concern to the wrapper
    // at registration time.

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
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .contains("Invalid glob pattern")
        );
    }

    #[tokio::test]
    async fn glob_search_lists_files_under_an_allowed_root_outside_the_workspace() {
        let workspace = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        std::fs::create_dir(project.path().join("docs")).unwrap();
        std::fs::write(project.path().join("docs/offer.md"), "offer").unwrap();
        let project_canon = std::fs::canonicalize(project.path()).unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_roots: vec![project_canon.clone()],
            workspace_only: false,
            ..SecurityPolicy::default()
        });
        let tool = GlobSearchTool::new(security);

        let pattern = format!("{}/docs/*.md", project_canon.display());
        let result = tool.execute(json!({ "pattern": pattern })).await.unwrap();

        assert!(result.success, "{:?}", result.error);
        let expected = project_canon.join("docs/offer.md");
        assert!(
            result.output.contains(&*expected.to_string_lossy()),
            "allowed-root match must be listed with its absolute path; got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn glob_search_lists_files_under_a_read_only_root() {
        let workspace = TempDir::new().unwrap();
        let shared = TempDir::new().unwrap();
        std::fs::write(shared.path().join("index.md"), "index").unwrap();
        let shared_canon = std::fs::canonicalize(shared.path()).unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_roots_read_only: vec![shared_canon.clone()],
            workspace_only: false,
            ..SecurityPolicy::default()
        });
        let tool = GlobSearchTool::new(security);

        let pattern = format!("{}/*.md", shared_canon.display());
        let result = tool.execute(json!({ "pattern": pattern })).await.unwrap();

        assert!(result.success, "{:?}", result.error);
        assert!(
            result
                .output
                .contains(&*shared_canon.join("index.md").to_string_lossy()),
            "read-only root match must be listed; got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn glob_search_never_lists_files_under_a_write_only_root() {
        let workspace = TempDir::new().unwrap();
        let drop_box = TempDir::new().unwrap();
        std::fs::write(drop_box.path().join("secret.txt"), "secret").unwrap();
        let drop_canon = std::fs::canonicalize(drop_box.path()).unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_roots_write_only: vec![drop_canon.clone()],
            workspace_only: false,
            ..SecurityPolicy::default()
        });
        let tool = GlobSearchTool::new(security);

        let pattern = format!("{}/*", drop_canon.display());
        let result = tool.execute(json!({ "pattern": pattern })).await.unwrap();

        assert!(
            !result.output.contains("secret.txt"),
            "write-only root must not be listed; got: {}",
            result.output
        );
    }

    // Symlink escape is a Unix-only concern; `std::os::unix::fs::symlink`
    // and the threat it exercises don't exist on Windows.
    #[cfg(unix)]
    #[tokio::test]
    async fn glob_search_filters_symlink_into_write_only_root() {
        let workspace = TempDir::new().unwrap();
        let sibling = TempDir::new().unwrap();
        std::fs::write(sibling.path().join("secret.txt"), "secret").unwrap();

        let symlink_path = workspace.path().join("siblings");
        std::os::unix::fs::symlink(sibling.path(), &symlink_path).unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_roots_write_only: vec![sibling.path().to_path_buf()],
            workspace_only: false,
            ..SecurityPolicy::default()
        });
        let tool = GlobSearchTool::new(security);

        let result = tool
            .execute(json!({"pattern": "siblings/*"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(
            !result.output.contains("secret.txt"),
            "write-only root must not surface through glob_search even via a workspace symlink; got: {}",
            result.output
        );
    }
}
