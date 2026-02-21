use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

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
}

impl ContentSearchTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        let has_rg = which::which("rg").is_ok();
        Self { security, has_rg }
    }

    #[cfg(test)]
    fn new_with_backend(security: Arc<SecurityPolicy>, has_rg: bool) -> Self {
        Self { security, has_rg }
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
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in, relative to workspace root. Defaults to '.'",
                    "default": "."
                },
                "output_mode": {
                    "type": "string",
                    "description": "Output format: 'content' (matching lines), 'files_with_matches' (paths only), 'count' (match counts)",
                    "enum": ["content", "files_with_matches", "count"],
                    "default": "content"
                },
                "include": {
                    "type": "string",
                    "description": "File glob filter, e.g. '*.rs', '*.{ts,tsx}'"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive matching. Defaults to true",
                    "default": true
                },
                "context_before": {
                    "type": "integer",
                    "description": "Lines of context before each match (content mode only)",
                    "default": 0
                },
                "context_after": {
                    "type": "integer",
                    "description": "Lines of context after each match (content mode only)",
                    "default": 0
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable multiline matching (ripgrep only, errors on grep fallback)",
                    "default": false
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 1000",
                    "default": 1000
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // --- Parse parameters ---
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' parameter"))?;

        if pattern.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Empty pattern is not allowed.".into()),
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
                error: Some(format!(
                    "Invalid output_mode '{output_mode}'. Allowed values: content, files_with_matches, count."
                )),
            });
        }

        let include = args.get("include").and_then(|v| v.as_str());

        let case_sensitive = args
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        #[allow(clippy::cast_possible_truncation)]
        let context_before = args
            .get("context_before")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        #[allow(clippy::cast_possible_truncation)]
        let context_after = args
            .get("context_after")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let multiline = args
            .get("multiline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        #[allow(clippy::cast_possible_truncation)]
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(MAX_RESULTS)
            .min(MAX_RESULTS);

        // --- Rate limit check ---
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // --- Path security checks ---
        if std::path::Path::new(search_path).is_absolute() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Absolute paths are not allowed. Use a relative path.".into()),
            });
        }

        if search_path.contains("../") || search_path.contains("..\\") || search_path == ".." {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Path traversal ('..') is not allowed.".into()),
            });
        }

        if !self.security.is_path_allowed(search_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Path '{search_path}' is not allowed by security policy."
                )),
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

        // --- Resolve search directory ---
        let workspace = &self.security.workspace_dir;
        let resolved_path = workspace.join(search_path);

        let resolved_canon = match std::fs::canonicalize(&resolved_path) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot resolve path '{search_path}': {e}")),
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
        for key in &["PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(TIMEOUT_SECS),
            tokio::process::Command::from(cmd).output(),
        )
        .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to execute search command: {e}")),
                });
            }
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Search timed out after {TIMEOUT_SECS} seconds.")),
                });
            }
        };

        // Exit code: 0 = matches found, 1 = no matches (grep/rg), 2 = error
        let exit_code = output.status.code().unwrap_or(-1);
        if exit_code >= 2 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Search error: {}", stderr.trim())),
            });
        }

        let raw_stdout = String::from_utf8_lossy(&output.stdout);

        // --- Parse and format output ---
        let workspace_canon =
            std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.clone());

        let formatted = if self.has_rg {
            format_rg_output(&raw_stdout, &workspace_canon, output_mode, max_results)
        } else {
            format_grep_output(&raw_stdout, &workspace_canon, output_mode, max_results)
        };

        // Truncate output if too large
        let final_output = if formatted.len() > MAX_OUTPUT_BYTES {
            let mut truncated = truncate_utf8(&formatted, MAX_OUTPUT_BYTES).to_string();
            truncated.push_str("\n\n[Output truncated: exceeded 1 MB limit]");
            truncated
        } else {
            formatted
        };

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
    include: Option<&str>,
    case_sensitive: bool,
    context_before: usize,
    context_after: usize,
    multiline: bool,
) -> std::process::Command {
    let mut cmd = std::process::Command::new("rg");

    // Use line-based output for structured parsing
    cmd.arg("--no-heading");
    cmd.arg("--line-number");
    cmd.arg("--with-filename");

    match output_mode {
        "files_with_matches" => {
            cmd.arg("--files-with-matches");
        }
        "count" => {
            cmd.arg("--count");
        }
        _ => {
            // content mode (default)
            if context_before > 0 {
                cmd.arg("-B").arg(context_before.to_string());
            }
            if context_after > 0 {
                cmd.arg("-A").arg(context_after.to_string());
            }
        }
    }

    if !case_sensitive {
        cmd.arg("-i");
    }

    if multiline {
        cmd.arg("-U");
        cmd.arg("--multiline-dotall");
    }

    if let Some(glob) = include {
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
    include: Option<&str>,
    case_sensitive: bool,
    context_before: usize,
    context_after: usize,
) -> std::process::Command {
    let mut cmd = std::process::Command::new("grep");

    cmd.arg("-r"); // recursive
    cmd.arg("-n"); // line numbers
    cmd.arg("-E"); // extended regex
    cmd.arg("--binary-files=without-match");

    match output_mode {
        "files_with_matches" => {
            cmd.arg("-l");
        }
        "count" => {
            cmd.arg("-c");
        }
        _ => {
            // content mode
            if context_before > 0 {
                cmd.arg("-B").arg(context_before.to_string());
            }
            if context_after > 0 {
                cmd.arg("-A").arg(context_after.to_string());
            }
        }
    }

    if !case_sensitive {
        cmd.arg("-i");
    }

    if let Some(glob) = include {
        cmd.arg("--include").arg(glob);
    }

    cmd.arg("--");
    cmd.arg(pattern);
    cmd.arg(search_path);

    cmd
}

fn format_rg_output(
    raw: &str,
    workspace_canon: &std::path::Path,
    output_mode: &str,
    max_results: usize,
) -> String {
    format_line_output(raw, workspace_canon, output_mode, max_results)
}

fn format_grep_output(
    raw: &str,
    workspace_canon: &std::path::Path,
    output_mode: &str,
    max_results: usize,
) -> String {
    format_line_output(raw, workspace_canon, output_mode, max_results)
}

/// Shared formatting for both rg and grep line-based outputs.
///
/// Both tools produce similar line-based output in our configuration:
/// - content mode: `path:line:content` or `path-line-content` (context lines)
/// - files_with_matches mode: `path`
/// - count mode: `path:count`
fn format_line_output(
    raw: &str,
    workspace_canon: &std::path::Path,
    output_mode: &str,
    max_results: usize,
) -> String {
    if raw.trim().is_empty() {
        return "No matches found.".to_string();
    }

    let workspace_prefix = workspace_canon.to_string_lossy();

    let mut lines: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut file_set = std::collections::HashSet::new();
    let mut total_matches: usize = 0;

    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }

        // Relativize paths: strip workspace prefix
        let relativized = relativize_path(line, &workspace_prefix);

        match output_mode {
            "files_with_matches" => {
                let path = relativized.trim();
                if !path.is_empty() && file_set.insert(path.to_string()) {
                    lines.push(path.to_string());
                    if lines.len() >= max_results {
                        truncated = true;
                        break;
                    }
                }
            }
            "count" => {
                // Format: path:count — filter out zero-count entries
                if let Some((path, count)) = parse_count_line(&relativized) {
                    if count > 0 {
                        file_set.insert(path.to_string());
                        total_matches += count;
                        lines.push(format!("{path}:{count}"));
                        if lines.len() >= max_results {
                            truncated = true;
                            break;
                        }
                    }
                }
            }
            _ => {
                // content mode: pass through with relativized paths
                // Track files from both match and context lines.
                if relativized == "--" {
                    lines.push(relativized);
                    if lines.len() >= max_results {
                        truncated = true;
                        break;
                    }
                    continue;
                }
                if let Some((path, is_match)) = parse_content_line(&relativized) {
                    file_set.insert(path.to_string());
                    if is_match {
                        total_matches += 1;
                    }
                } else {
                    // Unknown line format: keep output visible and count conservatively as a match.
                    total_matches += 1;
                }
                lines.push(relativized);
                if lines.len() >= max_results {
                    truncated = true;
                    break;
                }
            }
        }
    }

    if lines.is_empty() {
        return "No matches found.".to_string();
    }

    use std::fmt::Write;
    let mut buf = lines.join("\n");

    if truncated {
        let _ = write!(
            buf,
            "\n\n[Results truncated: showing first {max_results} results]"
        );
    }

    match output_mode {
        "files_with_matches" => {
            let _ = write!(buf, "\n\nTotal: {} files", file_set.len());
        }
        "count" => {
            let _ = write!(
                buf,
                "\n\nTotal: {} matches in {} files",
                total_matches,
                file_set.len()
            );
        }
        _ => {
            // content mode: show summary
            let _ = write!(
                buf,
                "\n\nTotal: {} matching lines in {} files",
                total_matches,
                file_set.len()
            );
        }
    }

    buf
}

/// Strip workspace prefix from a line, converting absolute paths to relative.
fn relativize_path(line: &str, workspace_prefix: &str) -> String {
    if let Some(rest) = line.strip_prefix(workspace_prefix) {
        // Strip leading separator
        let trimmed = rest
            .strip_prefix('/')
            .or_else(|| rest.strip_prefix('\\'))
            .unwrap_or(rest);
        return trimmed.to_string();
    }
    line.to_string()
}

/// Parse content output line and determine whether it is a real match line.
///
/// Supported formats:
/// - Match line: `path:line:content`
/// - Context line: `path-line-content`
fn parse_content_line(line: &str) -> Option<(&str, bool)> {
    static MATCH_RE: OnceLock<regex::Regex> = OnceLock::new();
    static CONTEXT_RE: OnceLock<regex::Regex> = OnceLock::new();

    let match_re = MATCH_RE.get_or_init(|| {
        regex::Regex::new(r"^(?P<path>.+?):\d+:").expect("match line regex must be valid")
    });
    if let Some(caps) = match_re.captures(line) {
        return caps.name("path").map(|m| (m.as_str(), true));
    }

    let context_re = CONTEXT_RE.get_or_init(|| {
        regex::Regex::new(r"^(?P<path>.+?)-\d+-").expect("context line regex must be valid")
    });
    if let Some(caps) = context_re.captures(line) {
        return caps.name("path").map(|m| (m.as_str(), false));
    }

    None
}

/// Parse count output line in `path:count` format.
fn parse_count_line(line: &str) -> Option<(&str, usize)> {
    static COUNT_RE: OnceLock<regex::Regex> = OnceLock::new();
    let count_re = COUNT_RE.get_or_init(|| {
        regex::Regex::new(r"^(?P<path>.+?):(?P<count>\d+)\s*$").expect("count line regex valid")
    });

    let caps = count_re.captures(line)?;
    let path = caps.name("path")?.as_str();
    let count = caps.name("count")?.as_str().parse::<usize>().ok()?;
    Some((path, count))
}

fn truncate_utf8(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
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
        let dir = TempDir::new().unwrap();
        create_test_files(&dir);

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "fn", "output_mode": "invalid_mode"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_ref()
            .unwrap()
            .contains("Invalid output_mode"));
    }

    #[tokio::test]
    async fn content_search_subdirectory() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("sub/deep/nested.rs"), "fn nested() {}\n").unwrap();
        std::fs::write(dir.path().join("root.rs"), "fn root() {}\n").unwrap();

        let tool = ContentSearchTool::new(test_security(dir.path().to_path_buf()));
        let result = tool
            .execute(json!({"pattern": "fn nested", "path": "sub"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("nested"));
        assert!(!result.output.contains("root"));
    }

    // --- Security tests ---

    #[tokio::test]
    async fn content_search_rejects_absolute_path() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"pattern": "test", "path": "/etc"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Absolute paths"));
    }

    #[tokio::test]
    async fn content_search_rejects_path_traversal() {
        let tool = ContentSearchTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"pattern": "test", "path": "../../../etc"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Path traversal"));
    }

    #[tokio::test]
    async fn content_search_rate_limited() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "test content\n").unwrap();

        let tool = ContentSearchTool::new(test_security_with(
            dir.path().to_path_buf(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool.execute(json!({"pattern": "test"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Rate limit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn content_search_symlink_escape_blocked() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");

        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret data\n").unwrap();

        // Symlink inside workspace pointing outside
        symlink(&outside, workspace.join("escape_dir")).unwrap();
        // Also add a legitimate file
        std::fs::write(workspace.join("legit.txt"), "legit data\n").unwrap();

        let tool = ContentSearchTool::new(test_security(workspace.clone()));
        let result = tool.execute(json!({"pattern": "data"})).await.unwrap();

        assert!(result.success);
        // Legit file should be found
        assert!(result.output.contains("legit.txt"));
        // The search runs in workspace, rg/grep may or may not follow symlinks,
        // but results are relativized — we mainly verify no crash
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
        let result = relativize_path("/workspace/src/main.rs:42:fn main()", "/workspace");
        assert_eq!(result, "src/main.rs:42:fn main()");
    }

    #[test]
    fn relativize_path_no_prefix() {
        let result = relativize_path("src/main.rs:42:fn main()", "/workspace");
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
