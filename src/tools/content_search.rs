use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const MAX_RESULTS: usize = 1000;
const MAX_OUTPUT_BYTES: usize = 1_048_576; // 1 MB
const TIMEOUT_SECS: u64 = 30;

/// Search file contents by regex pattern within the workspace.
///
/// Uses ripgrep (`rg`) when available, falling back to `grep -rn -E`.
/// All searches are confined to the workspace directory by security policy.
pub struct ContentSearchTool {
    security: Arc<SecurityPolicy>,
    has_rg: bool,
    has_grep: bool,
}

impl ContentSearchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        let has_rg = which::which("rg").is_ok();
        let has_grep = which::which("grep").is_ok();
        Self {
            security,
            has_rg,
            has_grep,
        }
    }

    #[cfg(test)]
    fn new_with_backend(security: Arc<SecurityPolicy>, has_rg: bool) -> Self {
        let has_grep = which::which("grep").is_ok();
        Self {
            security,
            has_rg,
            has_grep,
        }
    }

    fn has_any_backend(&self) -> bool {
        self.has_rg || self.has_grep
    }
}

#[async_trait]
impl Tool for ContentSearchTool {
    fn name(&self) -> &str {
        "content_search"
    }

    fn description(&self) -> &str {
        "Search file contents by regex pattern within the workspace. \
         Supports ripgrep (rg) with grep fallback. \
         Output modes: 'content' (matching lines with context), \
         'files_with_matches' (file paths only), 'count' (match counts per file). \
         Example: pattern='fn main', include='*.rs', output_mode='content'."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Subdirectory to search within (default: workspace root)",
                    "default": "."
                },
                "output_mode": {
                    "type": "string",
                    "description": "Output format: 'content' (default), 'files_with_matches', or 'count'",
                    "enum": ["content", "files_with_matches", "count"],
                    "default": "content"
                },
                "include": {
                    "type": "string",
                    "description": "File glob filter, e.g. '*.rs', '*.{ts,tsx}'"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether search should be case-sensitive",
                    "default": false
                },
                "context_before": {
                    "type": "integer",
                    "description": "Number of context lines before each match (output_mode='content' only)",
                    "default": 0
                },
                "context_after": {
                    "type": "integer",
                    "description": "Number of context lines after each match (output_mode='content' only)",
                    "default": 0
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable multiline matching (requires ripgrep)",
                    "default": false
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

        if pattern.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Empty pattern provided.".into()),
            });
        }

        let search_path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let output_mode = args
            .get("output_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("content");

        if !matches!(output_mode, "content" | "files_with_matches" | "count") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Invalid output_mode: {output_mode}")),
            });
        }

        let include = args.get("include").and_then(|v| v.as_str());

        let case_sensitive = args
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let context_before = args
            .get("context_before")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let context_after = args
            .get("context_after")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        let multiline = args
            .get("multiline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // --- Path Resolution & Security ---
        let resolved = self.security.workspace_dir.join(search_path);
        let resolved_canon = match resolved.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve path '{search_path}': {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_canon) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Resolved path for '{search_path}' is outside the allowed workspace."
                )),
            });
        }

        // --- Multiline check for grep fallback ---
        if multiline && !self.has_rg {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Multiline matching requires ripgrep (rg), which is not available.".into(),
                ),
            });
        }

        if !self.has_any_backend() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "No search backend available (ripgrep or grep not found in PATH).".into(),
                ),
            });
        }

        // --- Build and execute command ---
        let mut cmd = if self.has_rg {
            build_rg_command(
                pattern,
                &resolved_canon,
                output_mode,
                include,
                case_sensitive,
                context_before,
                context_after,
                multiline,
            )
        } else {
            build_grep_command(
                pattern,
                &resolved_canon,
                output_mode,
                include,
                case_sensitive,
                context_before,
                context_after,
            )
        };

        // Security: clear environment, keep only safe variables
        cmd.env_clear();
        for key in &[
            "PATH",
            "HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "SystemRoot",
            "windir",
            "USERPROFILE",
        ] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = match tokio::time::timeout(
            Duration::from_secs(TIMEOUT_SECS),
            cmd.spawn()
                .map_err(|e| anyhow::anyhow!("Failed to spawn search process: {e}"))?
                .wait_with_output(),
        )
        .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Search execution failed: {e}")),
                });
            }
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Search timed out after {TIMEOUT_SECS}s and was killed."
                    )),
                });
            }
        };

        if !output.status.success() && output.status.code() != Some(1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Search tool returned error: {stderr}")),
            });
        }

        let raw_stdout = String::from_utf8_lossy(&output.stdout);
        let final_output = format_line_output(
            &raw_stdout,
            &self.security.workspace_dir,
            output_mode,
            MAX_RESULTS,
        );

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        Ok(ToolResult {
            success: true,
            output: final_output,
            error: None,
        })
    }
}

fn build_rg_command(
    pattern: &str,
    search_path: &std::path::Path,
    output_mode: &str,
    glob: Option<&str>,
    case_sensitive: bool,
    context_before: usize,
    context_after: usize,
    multiline: bool,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("rg");
    cmd.arg("--line-number").arg("--with-filename");

    if !case_sensitive {
        cmd.arg("--ignore-case");
    }

    match output_mode {
        "files_with_matches" => {
            cmd.arg("--files-with-matches");
        }
        "count" => {
            cmd.arg("--count");
        }
        _ => {
            if context_before > 0 {
                cmd.arg("--before-context").arg(context_before.to_string());
            }
            if context_after > 0 {
                cmd.arg("--after-context").arg(context_after.to_string());
            }
            cmd.arg("--heading").arg("--break");
        }
    }

    if multiline {
        cmd.arg("--multiline");
    }

    if let Some(glob) = glob {
        cmd.arg("--glob").arg(glob);
    }

    // Separator to prevent pattern from being parsed as flag
    cmd.arg("--");
    cmd.arg(pattern);
    cmd.arg(search_path);

    cmd
}

fn build_grep_command(
    pattern: &str,
    search_path: &std::path::Path,
    output_mode: &str,
    glob: Option<&str>,
    case_sensitive: bool,
    context_before: usize,
    context_after: usize,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("grep");
    cmd.arg("-r").arg("-n").arg("-E");

    if !case_sensitive {
        cmd.arg("-i");
    }

    match output_mode {
        "files_with_matches" => {
            cmd.arg("-l");
        }
        "count" => {
            cmd.arg("-c");
        }
        _ => {
            if context_before > 0 {
                cmd.arg("-B").arg(context_before.to_string());
            }
            if context_after > 0 {
                cmd.arg("-A").arg(context_after.to_string());
            }
        }
    }

    if let Some(glob) = glob {
        cmd.arg("--include").arg(glob);
    }

    cmd.arg("--");
    cmd.arg(pattern);
    cmd.arg(search_path);

    cmd
}

fn relativize_path(line: &str, workspace: &std::path::Path) -> String {
    let workspace_str_raw = workspace.to_string_lossy();
    let workspace_str = workspace_str_raw.as_ref();

    // On Windows, canonicalize() often prepends \\?\ (UNC prefix)
    #[cfg(windows)]
    let workspace_str = workspace_str.strip_prefix(r"\\?\").unwrap_or(workspace_str);

    let mut result = line.to_string();
    #[cfg(windows)]
    if let Some(stripped) = result.strip_prefix(r"\\?\") {
        result = stripped.to_string();
    }

    // Try both forward and backward slashes for the workspace prefix
    let ws_prefix_f = if workspace_str.ends_with('/') {
        workspace_str.to_string()
    } else {
        format!("{}/", workspace_str)
    };
    let ws_prefix_b = if workspace_str.ends_with('\\') {
        workspace_str.to_string()
    } else {
        format!("{}\\", workspace_str)
    };

    result = result.replace(&ws_prefix_f, "");
    result = result.replace(&ws_prefix_b, "");
    result
}

fn format_line_output(
    raw: &str,
    workspace: &std::path::Path,
    output_mode: &str,
    max_results: usize,
) -> String {
    if raw.trim().is_empty() {
        return "No matches found.".into();
    }

    let mut lines: Vec<String> = raw
        .lines()
        .take(max_results)
        .map(|l| relativize_path(l, workspace))
        .collect();

    let count = lines.len();
    if count >= max_results {
        lines.push(format!(
            "\n... [truncated at {max_results} results, try a more specific pattern or path]"
        ));
    }

    match output_mode {
        "files_with_matches" => {
            let mut output = lines.join("\n");
            let _ = write!(output, "\n\nTotal: {count} files");
            output
        }
        "count" => {
            let mut output = lines.join("\n");
            let _ = write!(output, "\n\nTotal: matching counts per file");
            output
        }
        _ => {
            let mut output = lines.join("\n");

            // Count unique files and total matches
            let mut files = std::collections::HashSet::new();
            let mut matches_count = 0;

            for line in &lines {
                // rg/grep output format: "path:line:content" or "path-line-content" (context)
                // We only want to count actual matches (:) not context (-)

                // On Windows, the path might start with "C:\" - skip that first colon
                let search_start = if cfg!(windows)
                    && line.len() > 2
                    && line.as_bytes()[1] == b':'
                    && line.as_bytes()[0].is_ascii_alphabetic()
                {
                    2
                } else {
                    0
                };

                if let Some(colon_idx_rel) = line[search_start..].find(':') {
                    let colon_idx = search_start + colon_idx_rel;
                    let path_part = &line[..colon_idx];
                    if !path_part.is_empty() {
                        // Check if the next part is a line number
                        let after_path = &line[colon_idx + 1..];
                        if let Some(next_colon_idx) = after_path.find(':') {
                            let line_num_part = &after_path[..next_colon_idx];
                            if !line_num_part.is_empty()
                                && line_num_part.chars().all(|c| c.is_ascii_digit())
                            {
                                files.insert(path_part.to_string());
                                matches_count += 1;
                            }
                        }
                    }
                }
            }

            let _ = write!(
                output,
                "\n\nTotal: {matches_count} matching lines in {} files",
                files.len()
            );
            output
        }
    }
}

fn parse_count_line(line: &str) -> Option<(&str, usize)> {
    let parts: Vec<&str> = line.rsplitn(2, ':').collect();
    if parts.len() == 2 {
        let count = parts[0].parse::<usize>().ok()?;
        Some((parts[1], count))
    } else {
        None
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        let mut b = max_bytes;
        while b > 0 && !s.is_char_boundary(b) {
            b -= 1;
        }
        &s[..b]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use std::path::{Path, PathBuf};
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

    fn create_test_files(dir: &TempDir) {
        std::fs::write(
            dir.path().join("hello.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "pub fn greet() {\n    println!(\"greet\");\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("readme.txt"), "This is a readme file.\n").unwrap();
    }

    fn has_backend() -> bool {
        which::which("rg").is_ok() || which::which("grep").is_ok()
    }

    #[test]
    fn content_search_name_and_schema() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "content_search");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["pattern"].is_object());
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["output_mode"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("pattern")));
    }

    #[tokio::test]
    async fn content_search_basic_match() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        create_test_files(&dir);

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool.execute(json!({"pattern": "fn main"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello.rs"));
        assert!(result.output.contains("fn main"));
    }

    #[tokio::test]
    async fn content_search_files_with_matches_mode() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        create_test_files(&dir);

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "println", "output_mode": "files_with_matches"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello.rs"));
        assert!(result.output.contains("lib.rs"));
        assert!(!result.output.contains("readme.txt"));
        assert!(result.output.contains("Total: 2 files"));
    }

    #[tokio::test]
    async fn content_search_count_mode() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        create_test_files(&dir);

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "println", "output_mode": "count"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello.rs"));
        assert!(result.output.contains("lib.rs"));
        assert!(result.output.contains("Total:"));
    }

    #[tokio::test]
    async fn content_search_case_insensitive() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "Hello World\nhello world\n").unwrap();

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "HELLO", "case_sensitive": false}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("Hello World"));
        assert!(result.output.contains("hello world"));
    }

    #[tokio::test]
    async fn content_search_include_filter() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        create_test_files(&dir);

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "fn", "include": "*.rs"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("hello.rs"));
        assert!(!result.output.contains("readme.txt"));
    }

    #[tokio::test]
    async fn content_search_context_lines() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("ctx.rs"),
            "line1\nline2\ntarget_line\nline4\nline5\n",
        )
        .unwrap();

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "target_line", "context_before": 1, "context_after": 1}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("target_line"));
        assert!(result.output.contains("line2"));
        assert!(result.output.contains("line4"));
    }

    #[tokio::test]
    async fn content_search_no_matches() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        create_test_files(&dir);

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "nonexistent_string_xyz"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("No matches found"));
    }

    #[tokio::test]
    async fn content_search_empty_pattern_rejected() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"pattern": ""})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Empty pattern"));
    }

    #[tokio::test]
    async fn content_search_missing_pattern() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn content_search_invalid_output_mode_rejected() {
        if !has_backend() {
            return;
        }
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"pattern": "test", "output_mode": "invalid"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("output_mode"));
    }

    #[tokio::test]
    async fn content_search_rate_limited() {
        let tool = ContentSearchTool::new(test_security_with(
            std::env::temp_dir(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool.execute(json!({"pattern": "test"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Rate limit"));
    }

    #[tokio::test]
    async fn content_search_rejects_absolute_path() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));

        let abs_path = if cfg!(windows) { "C:\\etc" } else { "/etc" };

        let result = tool
            .execute(json!({"pattern": "test", "path": abs_path}))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.as_ref().unwrap();
        assert!(
            err.contains("Absolute paths") || err.contains("outside") || err.contains("resolve")
        );
    }

    #[tokio::test]
    async fn content_search_rejects_path_traversal() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"pattern": "test", "path": "../../../etc"}))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.as_ref().unwrap();
        assert!(err.contains("outside") || err.contains("resolve"));
    }

    #[tokio::test]
    async fn content_search_subdirectory() {
        if !has_backend() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("other.rs"), "fn other() {}\n").unwrap();

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "fn", "path": "src"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("main.rs"));
        assert!(!result.output.contains("other.rs"));
    }

    #[tokio::test]
    async fn content_search_multiline_without_rg() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "line1\nline2\n").unwrap();

        let tool = ContentSearchTool::new_with_backend(
            test_security(dir.path().to_path_buf()),
            false, // no rg
        );
        let result = tool
            .execute(json!({"pattern": "line1", "multiline": true}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("ripgrep"));
    }

    #[test]
    fn relativize_path_strips_prefix() {
        let result = relativize_path(
            "/workspace/src/main.rs:42:fn main()",
            Path::new("/workspace"),
        );
        assert_eq!(result, "src/main.rs:42:fn main()");
    }

    #[test]
    fn relativize_path_no_prefix() {
        let result = relativize_path("src/main.rs:42:fn main()", Path::new("/workspace"));
        assert_eq!(result, "src/main.rs:42:fn main()");
    }

    #[test]
    fn format_line_output_content_counts_match_lines_only() {
        let raw = "src/main.rs-1-use std::fmt;\nsrc/main.rs:2:fn main() {}\n--\nsrc/lib.rs:10:pub fn f() {}";
        let output = format_line_output(raw, std::path::Path::new("/workspace"), "content", 100);
        assert!(output.contains("Total: 2 matching lines in 2 files"));
    }

    #[test]
    fn parse_count_line_supports_colons_in_path() {
        let parsed = parse_count_line("dir:with:colon/file.rs:12");
        assert_eq!(parsed, Some(("dir:with:colon/file.rs", 12)));
    }

    #[test]
    fn truncate_utf8_keeps_char_boundary() {
        let text = "abc你好";
        // Byte index 4 splits the first Chinese character.
        let truncated = truncate_utf8(text, 4);
        assert_eq!(truncated, "abc");
    }
}
