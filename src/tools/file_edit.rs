use super::traits::{Tool, ToolResult};
use crate::security::file_link_guard::has_multiple_hard_links;
use crate::security::sensitive_paths::is_sensitive_file_path;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

// ── Whitespace-flexible matching helpers ─────────────────────────────

/// Byte range of a single line within file content.
struct LineSpan {
    /// Byte offset where the line text starts.
    text_start: usize,
    /// Byte offset where the line text ends (before `\r\n` or `\n`).
    text_end: usize,
    /// Byte offset after the line terminator (or `content.len()` for the last line).
    full_end: usize,
}

/// Result of the tiered matching strategy.
enum MatchOutcome {
    /// Exact substring match (handled separately, kept for completeness).
    Exact,
    /// Whitespace-flexible match found at byte range `[start, end)`.
    WhitespaceFlexible { start: usize, end: usize },
    /// Multiple matches found — ambiguous.
    Ambiguous { count: usize, tier: &'static str },
    /// No match at any tier.
    NotFound,
}

/// Normalize a line for whitespace-flexible comparison:
/// - Collapse every run of spaces/tabs into a single space.
/// - Trim trailing whitespace.
fn normalize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_ws = false;
    for ch in line.chars() {
        if ch == ' ' || ch == '\t' {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    // Trim trailing whitespace (the collapsed trailing space, if any).
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    out
}

/// Split `content` into per-line byte spans, handling `\n` and `\r\n`.
fn compute_line_spans(content: &str) -> Vec<LineSpan> {
    let bytes = content.as_bytes();
    let mut spans = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let text_start = pos;
        // Scan to next newline.
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
        // `pos` is at `\n` or end-of-content.
        let text_end = if pos > text_start && bytes[pos - 1] == b'\r' {
            pos - 1
        } else {
            pos
        };
        let full_end = if pos < bytes.len() { pos + 1 } else { pos };
        spans.push(LineSpan {
            text_start,
            text_end,
            full_end,
        });
        pos = full_end;
    }
    // Handle trailing empty content (empty file produces no spans but callers cope).
    spans
}

/// Attempt whitespace-flexible line matching of `old_string` within `content`.
///
/// Algorithm:
/// 1. Normalize each line of `old_string` and `content`.
/// 2. Slide a window of `old_lines.len()` across content lines.
/// 3. Compare normalized lines pairwise.
/// 4. Return outcome based on match count.
fn try_flexible_line_match(content: &str, old_string: &str) -> MatchOutcome {
    let old_lines: Vec<String> = old_string.lines().map(normalize_line).collect();
    if old_lines.is_empty() {
        return MatchOutcome::NotFound;
    }

    let spans = compute_line_spans(content);
    let content_normalized: Vec<String> = spans
        .iter()
        .map(|s| normalize_line(&content[s.text_start..s.text_end]))
        .collect();

    let window_size = old_lines.len();
    if window_size > spans.len() {
        return MatchOutcome::NotFound;
    }

    let mut matches: Vec<(usize, usize)> = Vec::new();

    for i in 0..=(spans.len() - window_size) {
        if content_normalized[i..i + window_size] == old_lines[..] {
            let start = spans[i].text_start;
            // For the end boundary: if old_string ends with `\n`, include the
            // line terminator of the last matched line; otherwise use text_end.
            let end = if old_string.ends_with('\n') || old_string.ends_with("\r\n") {
                spans[i + window_size - 1].full_end
            } else {
                spans[i + window_size - 1].text_end
            };
            matches.push((start, end));
        }
    }

    match matches.len() {
        0 => MatchOutcome::NotFound,
        1 => MatchOutcome::WhitespaceFlexible {
            start: matches[0].0,
            end: matches[0].1,
        },
        n => MatchOutcome::Ambiguous {
            count: n,
            tier: "whitespace-normalized",
        },
    }
}

/// Edit a file by replacing a string match with new content.
///
/// Uses exact matching first; falls back to whitespace-flexible line matching
/// when exact match fails. The `old_string` must match exactly once at any tier
/// (zero = not found, multiple = ambiguous). `new_string` may be empty to delete
/// the matched text. Security checks mirror [`super::file_write::FileWriteTool`].
pub struct FileEditTool {
    security: Arc<SecurityPolicy>,
}

impl FileEditTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

fn sensitive_file_edit_block_message(path: &str) -> String {
    format!(
        "Editing sensitive file '{path}' is blocked by policy. \
Set [autonomy].allow_sensitive_file_writes = true only when strictly necessary."
    )
}

fn hard_link_edit_block_message(path: &Path) -> String {
    format!(
        "Editing multiply-linked file '{}' is blocked by policy \
(potential hard-link escape).",
        path.display()
    )
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing a string match with new content. \
         Uses exact matching first; falls back to whitespace-flexible \
         line matching when exact match fails. Sensitive files (for example \
         .env and key material) are blocked by default."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths resolve from workspace; outside paths require policy allowlist."
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

        // ── 4. Path pre-validation ─────────────────────────────────
        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        if !self.security.allow_sensitive_file_writes && is_sensitive_file_path(Path::new(path)) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(sensitive_file_edit_block_message(path)),
            });
        }

        let full_path = self.security.workspace_dir.join(path);

        // ── 5. Canonicalize parent ─────────────────────────────────
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

        // ── 6. Resolved path post-validation ───────────────────────
        if !self.security.is_resolved_path_allowed(&resolved_parent) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&resolved_parent),
                ),
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

        if !self.security.allow_sensitive_file_writes && is_sensitive_file_path(&resolved_target) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(sensitive_file_edit_block_message(
                    &resolved_target.display().to_string(),
                )),
            });
        }

        // ── 7. Symlink check ───────────────────────────────────────
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await {
            if meta.file_type().is_symlink() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Refusing to edit through symlink: {}",
                        resolved_target.display()
                    )),
                });
            }

            if has_multiple_hard_links(&meta) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(hard_link_edit_block_message(&resolved_target)),
                });
            }
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

        let (new_content, matched_flexible) = match match_count.cmp(&1) {
            std::cmp::Ordering::Equal => {
                // Tier 1: exact match — fast path, zero overhead.
                (content.replacen(old_string, new_string, 1), false)
            }
            std::cmp::Ordering::Greater => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "old_string matches {match_count} times; must match exactly once"
                    )),
                });
            }
            std::cmp::Ordering::Less => {
                // Tier 2: whitespace-flexible line matching fallback.
                match try_flexible_line_match(&content, old_string) {
                    MatchOutcome::WhitespaceFlexible { start, end } => {
                        let mut buf =
                            String::with_capacity(content.len() - (end - start) + new_string.len());
                        buf.push_str(&content[..start]);
                        buf.push_str(new_string);
                        buf.push_str(&content[end..]);
                        (buf, true)
                    }
                    MatchOutcome::Ambiguous { count, tier } => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!(
                                "old_string matches {count} times with {tier} matching; \
                                 must match exactly once"
                            )),
                        });
                    }
                    MatchOutcome::NotFound | MatchOutcome::Exact => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("old_string not found in file".into()),
                        });
                    }
                }
            }
        };

        let flexibility_note = if matched_flexible {
            " (matched with whitespace flexibility)"
        } else {
            ""
        };

        match tokio::fs::write(&resolved_target, &new_content).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Edited {path}: replaced 1 occurrence ({} bytes){flexibility_note}",
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
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with(
        workspace: std::path::PathBuf,
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

    fn test_security_allow_sensitive_writes(
        workspace: std::path::PathBuf,
        allow_sensitive_file_writes: bool,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            allow_sensitive_file_writes,
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn file_edit_name() {
        let tool = FileEditTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "file_edit");
    }

    #[test]
    fn file_edit_schema_has_required_params() {
        let tool = FileEditTool::new(test_security(std::env::temp_dir()));
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

        let tool = FileEditTool::new(test_security(dir.clone()));
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

        let tool = FileEditTool::new(test_security(dir.clone()));
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

        // File should be unchanged
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

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "aaa",
                "new_string": "ccc"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("matches 2 times"));

        // File should be unchanged
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

        let tool = FileEditTool::new(test_security(dir.clone()));
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
    async fn file_edit_blocks_sensitive_file_by_default() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_sensitive_blocked");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(".env"), "API_KEY=old")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": ".env",
                "old_string": "old",
                "new_string": "new"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("sensitive file"));

        let content = tokio::fs::read_to_string(dir.join(".env")).await.unwrap();
        assert_eq!(content, "API_KEY=old");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_allows_sensitive_file_when_configured() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_sensitive_allowed");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(".env"), "API_KEY=old")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security_allow_sensitive_writes(dir.clone(), true));
        let result = tool
            .execute(json!({
                "path": ".env",
                "old_string": "old",
                "new_string": "new"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "sensitive edit should succeed when enabled: {:?}",
            result.error
        );

        let content = tokio::fs::read_to_string(dir.join(".env")).await.unwrap();
        assert_eq!(content, "API_KEY=new");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_missing_path_param() {
        let tool = FileEditTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"old_string": "a", "new_string": "b"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_missing_old_string_param() {
        let tool = FileEditTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"path": "f.txt", "new_string": "b"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_missing_new_string_param() {
        let tool = FileEditTool::new(test_security(std::env::temp_dir()));
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

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "",
                "new_string": "x"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("must not be empty"));

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

        let tool = FileEditTool::new(test_security(dir.clone()));
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
        let tool = FileEditTool::new(test_security(std::env::temp_dir()));
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

        let tool = FileEditTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({
                "path": "escape_dir/target.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("escapes workspace"));

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

        let tool = FileEditTool::new(test_security(workspace.clone()));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn file_edit_blocks_hardlink_target_file() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_edit_hardlink_target");
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        tokio::fs::write(outside.join("target.txt"), "original")
            .await
            .unwrap();
        std::fs::hard_link(outside.join("target.txt"), workspace.join("linked.txt")).unwrap();

        let tool = FileEditTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({
                "path": "linked.txt",
                "old_string": "original",
                "new_string": "hacked"
            }))
            .await
            .unwrap();

        assert!(!result.success, "editing through hard link must be blocked");
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("hard-link escape"));

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

        let tool = FileEditTool::new(test_security_with(dir.clone(), AutonomyLevel::ReadOnly, 20));
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

        let tool = FileEditTool::new(test_security_with(
            dir.clone(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "hello",
                "new_string": "world"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));

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

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "missing.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to read file"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_blocks_null_byte_in_path() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_null_byte");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
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

    // ── Whitespace-flexible matching tests ───────────────────────────

    #[tokio::test]
    async fn file_edit_flexible_matches_different_indentation() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_indent");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // File has 4-space indentation.
        tokio::fs::write(
            dir.join("test.rs"),
            "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        )
        .await
        .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        // old_string uses 2-space indentation — exact match fails.
        let result = tool
            .execute(json!({
                "path": "test.rs",
                "old_string": "  let x = 1;\n  let y = 2;",
                "new_string": "    let x = 10;\n    let y = 20;"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "flexible match should succeed: {:?}",
            result.error
        );
        assert!(result.output.contains("whitespace flexibility"));

        let content = tokio::fs::read_to_string(dir.join("test.rs"))
            .await
            .unwrap();
        assert_eq!(
            content,
            "fn main() {\n    let x = 10;\n    let y = 20;\n}\n"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_flexible_matches_tabs_vs_spaces() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_tabs");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // File uses tabs.
        tokio::fs::write(dir.join("test.py"), "def foo():\n\treturn 42\n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        // old_string uses spaces.
        let result = tool
            .execute(json!({
                "path": "test.py",
                "old_string": "    return 42",
                "new_string": "\treturn 99"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "tab vs space flexible match should succeed: {:?}",
            result.error
        );
        assert!(result.output.contains("whitespace flexibility"));

        let content = tokio::fs::read_to_string(dir.join("test.py"))
            .await
            .unwrap();
        assert_eq!(content, "def foo():\n\treturn 99\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_flexible_matches_trailing_whitespace() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_trailing");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // File has trailing spaces on lines — multi-line old_string so exact
        // substring match fails (trailing spaces break the exact match).
        tokio::fs::write(dir.join("test.txt"), "line one  \nline two  \n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        // old_string has no trailing spaces — exact match won't find it.
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "line one\nline two",
                "new_string": "line ONE\nline TWO"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "trailing whitespace flexible match should succeed: {:?}",
            result.error
        );
        assert!(result.output.contains("whitespace flexibility"));

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "line ONE\nline TWO\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_flexible_matches_multiple_spaces() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_multispaces");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // File has double spaces between words.
        tokio::fs::write(dir.join("test.txt"), "a  b  c\n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        // old_string uses single spaces.
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "a b c",
                "new_string": "x y z"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "multiple-space flexible match should succeed: {:?}",
            result.error
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "x y z\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_flexible_ambiguous() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_ambiguous");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // Two lines that normalize identically.
        tokio::fs::write(dir.join("test.txt"), "  hello\n\thello\n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "    hello",
                "new_string": "world"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("whitespace-normalized"));

        // File unchanged.
        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "  hello\n\thello\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_flexible_not_found() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_notfound");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "alpha\nbeta\n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "  gamma",
                "new_string": "delta"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("not found"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_flexible_preserves_surrounding_content() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_flex_surround");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // File uses tab indent — old_string uses spaces, so no exact substring.
        tokio::fs::write(dir.join("test.txt"), "before\n\ttarget line\nafter\n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        // old_string uses spaces instead of tab — exact match fails.
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "    target line",
                "new_string": "\treplaced line"
            }))
            .await
            .unwrap();

        assert!(result.success, "should succeed: {:?}", result.error);

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "before\n\treplaced line\nafter\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_edit_exact_match_preferred_over_flexible() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_edit_exact_pref");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "  hello world\n")
            .await
            .unwrap();

        let tool = FileEditTool::new(test_security(dir.clone()));
        // old_string matches exactly — should NOT report flexibility.
        let result = tool
            .execute(json!({
                "path": "test.txt",
                "old_string": "  hello world",
                "new_string": "  goodbye world"
            }))
            .await
            .unwrap();

        assert!(
            result.success,
            "exact match should succeed: {:?}",
            result.error
        );
        assert!(
            !result.output.contains("whitespace flexibility"),
            "should not report flexibility for exact match"
        );

        let content = tokio::fs::read_to_string(dir.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "  goodbye world\n");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
