use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Configuration for generating signed download URLs in tool output.
#[derive(Clone)]
pub struct DownloadUrlConfig {
    /// Public base URL of the gateway (e.g. `https://myhost.ts.net:42617`).
    pub base_url: String,
    /// HMAC secret for signing download URLs.
    pub secret: Vec<u8>,
}

/// Write file contents with path sandboxing
pub struct FileWriteTool {
    security: Arc<SecurityPolicy>,
    download: Option<DownloadUrlConfig>,
}

impl FileWriteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            download: None,
        }
    }

    /// Create with download URL generation support.
    pub fn with_download(security: Arc<SecurityPolicy>, download: DownloadUrlConfig) -> Self {
        Self {
            security,
            download: Some(download),
        }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write contents to a file in the workspace"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths resolve from workspace; outside paths require policy allowlist."
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;

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

        // Security check: validate path is within workspace
        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        let full_path = self.security.workspace_dir.join(path);

        let Some(parent) = full_path.parent() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid path: missing parent directory".into()),
            });
        };

        // Ensure parent directory exists
        tokio::fs::create_dir_all(parent).await?;

        // Resolve parent AFTER creation to block symlink escapes.
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

        // If the target already exists and is a symlink, refuse to follow it
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await {
            if meta.file_type().is_symlink() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Refusing to write through symlink: {}",
                        resolved_target.display()
                    )),
                });
            }
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        match tokio::fs::write(&resolved_target, content).await {
            Ok(()) => {
                let mut output = format!("Written {} bytes to {path}", content.len());
                // Always sign the relative path from workspace_dir so gateway routing works stably.
                //
                // macOS note: `canonicalize(parent)` above resolves symlinks like
                // `/var/folders/...` → `/private/var/folders/...`, so stripping
                // against the RAW workspace_dir fails and falls back to an
                // absolute path (which then gets percent-encoded into a broken
                // signed URL like `/download/%2Fprivate%2F...`). We canonicalize
                // the workspace path here to match.
                let canonical_workspace = tokio::fs::canonicalize(&self.security.workspace_dir)
                    .await
                    .unwrap_or_else(|_| self.security.workspace_dir.clone());
                let relative_path = resolved_target
                    .strip_prefix(&canonical_workspace)
                    .unwrap_or(&resolved_target);
                let rel_str = relative_path.to_string_lossy().to_string();
                // Gate artifact + Download: URL emission on the user-deliverable
                // whitelist. Skills routinely `file_write` intermediate scripts
                // (`.py`/`.js`/`.sh`) before running them via `shell` — surfacing
                // those as downloads spams chat clients with internals and, on
                // Lark, results in a `.py` attachment next to the real `.docx`.
                // Files outside the whitelist still succeed, just silently.
                let is_deliverable = crate::tools::artifact::is_artifact_extension(&rel_str);
                let download_url = if is_deliverable {
                    self.download.as_ref().map(|dl| {
                        crate::gateway::signed_url::sign_download_url(
                            &dl.base_url,
                            &rel_str,
                            &dl.secret,
                            crate::gateway::signed_url::DEFAULT_TTL_SECS,
                        )
                    })
                } else {
                    None
                };
                // Legacy `Download:` line — kept for the regex-based fallback in
                // `channels/mod.rs::extract_download_urls_from_history` and
                // `lark::extract_download_links`. Non-deliverable extensions
                // skip this entirely.
                if let Some(url) = download_url.as_deref() {
                    use std::fmt::Write;
                    let _ = write!(output, "\nDownload: {url}");
                }
                // Structured artifact (new contract). Emitted only for deliverable
                // extensions AND when `download` is configured.
                if is_deliverable && download_url.is_some() {
                    let artifacts: Vec<crate::tools::artifact::Artifact> =
                        crate::tools::artifact::Artifact::from_workspace_path(
                            &canonical_workspace,
                            &rel_str,
                            download_url,
                        )
                        .into_iter()
                        .collect();
                    crate::tools::artifact::append_artifacts(&mut output, &artifacts);
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
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

    #[test]
    fn file_write_name() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "file_write");
    }

    #[test]
    fn file_write_schema_has_path_and_content() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["content"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("content")));
    }

    #[tokio::test]
    async fn file_write_creates_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "out.txt", "content": "written!"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("8 bytes"));

        let content = tokio::fs::read_to_string(dir.join("out.txt"))
            .await
            .unwrap();
        assert_eq!(content, "written!");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_creates_parent_dirs() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_nested");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "a/b/c/deep.txt", "content": "deep"}))
            .await
            .unwrap();
        assert!(result.success);

        let content = tokio::fs::read_to_string(dir.join("a/b/c/deep.txt"))
            .await
            .unwrap();
        assert_eq!(content, "deep");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_overwrites_existing() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_overwrite");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("exist.txt"), "old")
            .await
            .unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "exist.txt", "content": "new"}))
            .await
            .unwrap();
        assert!(result.success);

        let content = tokio::fs::read_to_string(dir.join("exist.txt"))
            .await
            .unwrap();
        assert_eq!(content, "new");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_path_traversal() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_traversal");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "../../etc/evil", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_absolute_path() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let result = tool
            .execute(json!({"path": "/etc/evil", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn file_write_missing_path_param() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"content": "data"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_write_missing_content_param() {
        let tool = FileWriteTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"path": "file.txt"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_write_empty_content() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_empty");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "empty.txt", "content": ""}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("0 bytes"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_file_write_symlink_escape");
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        symlink(&outside, workspace.join("escape_dir")).unwrap();

        let tool = FileWriteTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({"path": "escape_dir/hijack.txt", "content": "bad"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("escapes workspace"));
        assert!(!outside.join("hijack.txt").exists());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_write_blocks_readonly_mode() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_readonly");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security_with(dir.clone(), AutonomyLevel::ReadOnly, 20));
        let result = tool
            .execute(json!({"path": "out.txt", "content": "should-block"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("read-only"));
        assert!(!dir.join("out.txt").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_when_rate_limited() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_rate_limited");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security_with(
            dir.clone(),
            AutonomyLevel::Supervised,
            0,
        ));
        let result = tool
            .execute(json!({"path": "out.txt", "content": "should-block"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
        assert!(!dir.join("out.txt").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── §5.1 TOCTOU / symlink file write protection tests ────

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_blocks_symlink_target_file() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_file_write_symlink_target");
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        // Create a file outside and symlink to it inside workspace
        tokio::fs::write(outside.join("target.txt"), "original")
            .await
            .unwrap();
        symlink(outside.join("target.txt"), workspace.join("linked.txt")).unwrap();

        let tool = FileWriteTool::new(test_security(workspace.clone()));
        let result = tool
            .execute(json!({"path": "linked.txt", "content": "overwritten"}))
            .await
            .unwrap();

        assert!(!result.success, "writing through symlink must be blocked");
        assert!(
            result.error.as_deref().unwrap_or("").contains("symlink"),
            "error should mention symlink"
        );

        // Verify original file was not modified
        let content = tokio::fs::read_to_string(outside.join("target.txt"))
            .await
            .unwrap();
        assert_eq!(content, "original", "original file must not be modified");

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_write_blocks_null_byte_in_path() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_null");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "file\u{0000}.txt", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success, "paths with null bytes must be blocked");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// PR 1 contract: when `with_download` is configured AND the file is a
    /// user deliverable (whitelisted extension), file_write emits BOTH
    /// (a) the legacy `Download:` text line for the regex fallback pipeline
    /// AND (b) a structured artifact via the sentinel codec.
    #[tokio::test]
    async fn file_write_with_download_emits_both_legacy_and_artifact_for_deliverable() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_artifact");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::with_download(
            test_security(dir.clone()),
            DownloadUrlConfig {
                base_url: "https://gw.example".into(),
                secret: b"test-secret".to_vec(),
            },
        );
        // `.docx` is on the user-deliverable whitelist.
        let result = tool
            .execute(json!({"path": "report.docx", "content": "PK\u{3}\u{4}"}))
            .await
            .unwrap();
        assert!(result.success);

        assert!(
            result
                .output
                .contains("\nDownload: https://gw.example/download/report.docx?expires="),
            "legacy Download: line missing for a deliverable extension: {:?}",
            result.output
        );

        let (cleaned, artifacts) = crate::tools::artifact::extract_artifacts(&result.output);
        assert!(!cleaned.contains("zeroclaw-artifacts"));
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].path, "report.docx");
        assert_eq!(artifacts[0].name, "report.docx");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Regression guard for the spam-intermediates bug: writing a non-
    /// deliverable file (e.g. a Python script a skill is about to run)
    /// must NOT emit a Download: line or an artifact, even when download
    /// config is wired. Otherwise chats get a `generate.py` attachment
    /// next to the real deliverable.
    #[tokio::test]
    async fn file_write_suppresses_artifact_for_intermediate_extensions() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_intermediate");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::with_download(
            test_security(dir.clone()),
            DownloadUrlConfig {
                base_url: "https://gw.example".into(),
                secret: b"test-secret".to_vec(),
            },
        );
        // `.py` is intentionally NOT on the whitelist.
        let result = tool
            .execute(json!({"path": "generate_docx.py", "content": "print('hi')"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            !result.output.contains("Download:"),
            "intermediate .py must not emit Download: line — would spam chat clients"
        );
        let (_cleaned, artifacts) = crate::tools::artifact::extract_artifacts(&result.output);
        assert!(
            artifacts.is_empty(),
            "intermediate .py must not emit a structured artifact"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Without `with_download`, no sentinel and no `Download:` line — keeps
    /// behaviour identical to pre-PR-1 for minimal-config deployments.
    #[tokio::test]
    async fn file_write_without_download_emits_no_artifact() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_no_artifact");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = FileWriteTool::new(test_security(dir.clone()));
        let result = tool
            .execute(json!({"path": "x.md", "content": "z"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(!result.output.contains("Download:"));
        let (_, artifacts) = crate::tools::artifact::extract_artifacts(&result.output);
        assert!(artifacts.is_empty());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
