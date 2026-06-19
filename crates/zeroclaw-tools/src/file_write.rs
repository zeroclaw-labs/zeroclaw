use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Write file contents with path sandboxing
pub struct FileWriteTool {
    security: Arc<SecurityPolicy>,
    /// Whether writes to the workspace will persist on the host filesystem.
    /// `false` when the runtime uses an ephemeral sandbox (e.g. Docker without
    /// a workspace volume mount), in which case writes succeed inside the
    /// container but are invisible on the host.
    persistent_writes: bool,
}

impl FileWriteTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            persistent_writes: true,
        }
    }

    /// Construct with an explicit persistence flag derived from the active
    /// runtime adapter's `has_filesystem_access()`.
    pub fn new_with_persistence(security: Arc<SecurityPolicy>, persistent_writes: bool) -> Self {
        Self {
            security,
            persistent_writes,
        }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write contents to a file in the workspace. Text by default; set encoding=\"base64\" to write binary files (e.g. .xlsx/.docx) by decoding base64 content into raw bytes."
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
                    "description": "Content to write. UTF-8 text when encoding is 'utf8'; base64-encoded bytes when encoding is 'base64'."
                },
                "encoding": {
                    "type": "string",
                    "enum": ["utf8", "base64"],
                    "description": "How to interpret 'content' before writing (default: 'utf8'). Use 'base64' for binary files."
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "path"})),
                "file_write: missing path parameter"
            );
            anyhow::Error::msg("Missing 'path' parameter")
        })?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "content"})),
                    "file_write: missing content parameter"
                );
                anyhow::Error::msg("Missing 'content' parameter")
            })?;

        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8");

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.persistent_writes {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "file_write is unavailable: the active runtime uses an ephemeral workspace \
                     (tmpfs / no host volume mount). Files written here would not persist on the \
                     host after the session ends. To fix this, set \
                     `runtime.docker.mount_workspace = true` in your config and ensure the \
                     workspace directory is bind-mounted into the container."
                        .into(),
                ),
            });
        }

        // Validate the encoding and decode base64 BEFORE any write-side
        // filesystem mutation (e.g. parent directory creation), so invalid
        // input fails without touching the workspace. Path-sandbox checks
        // below still run on the resolved target before the write.
        let bytes = match encoding {
            "utf8" => content.as_bytes().to_vec(),
            "base64" => {
                use base64::Engine;
                match base64::engine::general_purpose::STANDARD.decode(content) {
                    Ok(decoded) => decoded,
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Invalid base64 content: {e}")),
                        });
                    }
                }
            }
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unsupported encoding '{other}' (expected 'utf8' or 'base64')"
                    )),
                });
            }
        };

        // Rate limiting and path-allowlist checks are applied by the
        // RateLimitedTool + PathGuardedTool wrappers at registration time
        // (see zeroclaw-runtime::tools::mod).

        let full_path = self.security.resolve_tool_path(path);

        let Some(parent) = full_path.parent() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Invalid path: missing parent directory".into()),
            });
        };

        // Ensure parent directory exists before canonicalising.
        tokio::fs::create_dir_all(parent).await?;

        // Canonicalise parent AFTER creation to detect symlink escapes.
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

        if self.security.is_runtime_config_path(&resolved_target) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    self.security
                        .runtime_config_violation_message(&resolved_target),
                ),
            });
        }

        // If the target already exists and is a symlink, refuse to follow it
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await
            && meta.file_type().is_symlink()
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Refusing to write through symlink: {}",
                    resolved_target.display()
                )),
            });
        }

        match tokio::fs::write(&resolved_target, &bytes).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Written {} bytes to {path}", bytes.len()),
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
    use crate::wrappers::{PathGuardedTool, RateLimitedTool};
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool(workspace: std::path::PathBuf) -> FileWriteTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        FileWriteTool::new(security)
    }

    /// Wraps `FileWriteTool` with the production `PathGuardedTool` + `RateLimitedTool`
    /// stack, mirroring the registration in `zeroclaw-runtime::tools::mod`. Use this
    /// in tests that exercise path-allowlist or rate-limit behavior.
    fn wrapped_tool(workspace: std::path::PathBuf) -> Box<dyn Tool> {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(FileWriteTool::new(security.clone()), security.clone()),
            security,
        ))
    }

    fn test_tool_with(
        workspace: std::path::PathBuf,
        autonomy: AutonomyLevel,
        max_actions_per_hour: u32,
    ) -> FileWriteTool {
        let security = Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: workspace,
            max_actions_per_hour,
            ..SecurityPolicy::default()
        });
        FileWriteTool::new(security)
    }

    fn ephemeral_tool(workspace: std::path::PathBuf) -> FileWriteTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        FileWriteTool::new_with_persistence(security, false)
    }

    #[cfg(target_os = "windows")]
    fn absolute_path_outside_workspace() -> &'static str {
        r"C:\Windows\win.ini"
    }

    #[cfg(not(target_os = "windows"))]
    fn absolute_path_outside_workspace() -> &'static str {
        "/etc/evil"
    }

    #[test]
    fn file_write_name() {
        let tool = test_tool(std::env::temp_dir());
        assert_eq!(tool.name(), "file_write");
    }

    #[test]
    fn file_write_schema_has_path_and_content() {
        let tool = test_tool(std::env::temp_dir());
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

        let tool = test_tool(dir.clone());
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

        let tool = test_tool(dir.clone());
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
    #[cfg(unix)]
    async fn file_write_normalizes_workspace_prefixed_relative_path() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_write_workspace_prefixed");
        let workspace = root.join("workspace");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let tool = test_tool(workspace.clone());
        let workspace_prefixed = workspace
            .strip_prefix(std::path::Path::new("/"))
            .unwrap()
            .join("nested/out.txt");
        let result = tool
            .execute(json!({
                "path": workspace_prefixed.to_string_lossy(),
                "content": "written!"
            }))
            .await
            .unwrap();
        assert!(result.success);

        let content = tokio::fs::read_to_string(workspace.join("nested/out.txt"))
            .await
            .unwrap();
        assert_eq!(content, "written!");
        assert!(!workspace.join(workspace_prefixed).exists());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_write_overwrites_existing() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_overwrite");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("exist.txt"), "old")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
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

        let tool = wrapped_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "../../etc/evil", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_ref().unwrap().contains("Path blocked"),
            "expected 'Path blocked' error, got: {:?}",
            result.error
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_absolute_path() {
        let tool = wrapped_tool(std::env::temp_dir());
        let result = tool
            .execute(json!({"path": absolute_path_outside_workspace(), "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_ref().unwrap().contains("Path blocked"),
            "expected 'Path blocked' error, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn file_write_missing_path_param() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool.execute(json!({"content": "data"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_write_missing_content_param() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool.execute(json!({"path": "file.txt"})).await;
        assert!(result.is_err());
    }

    #[test]
    fn file_write_schema_has_encoding() {
        let tool = test_tool(std::env::temp_dir());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["encoding"].is_object());
    }

    #[tokio::test]
    async fn file_write_base64_writes_decoded_bytes() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_base64");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Bytes that are NOT valid UTF-8 — proves we persist raw bytes, not text.
        let raw: Vec<u8> = vec![0x00, 0x01, 0xFF, 0xFE, b'P', b'K', 0x03, 0x04];
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "out.bin", "content": encoded, "encoding": "base64"}))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(result.output.contains(&format!("{} bytes", raw.len())));

        let written = tokio::fs::read(dir.join("out.bin")).await.unwrap();
        assert_eq!(written, raw, "base64 write must persist exact raw bytes");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_base64_invalid_content_errors() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_base64_invalid");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(
                json!({"path": "out.bin", "content": "not!valid!base64!", "encoding": "base64"}),
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Invalid base64")
        );
        assert!(
            !dir.join("out.bin").exists(),
            "no file must be written on decode failure"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_unsupported_encoding_errors() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_bad_encoding");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "out.txt", "content": "hi", "encoding": "hex"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Unsupported encoding")
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Rejected writes (invalid base64 / unsupported encoding) targeting a
    /// missing nested parent must fail WITHOUT mutating the workspace — no
    /// file and, crucially, no parent directory may be created.
    #[tokio::test]
    async fn file_write_rejected_encoding_does_not_create_parent_dirs() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_no_dir_on_reject");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());

        // Invalid base64 into a missing nested parent.
        let result = tool
            .execute(json!({
                "path": "nested/out.bin",
                "content": "not!valid!base64!",
                "encoding": "base64"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Invalid base64")
        );
        assert!(
            !dir.join("nested").exists(),
            "rejected base64 write must not create the parent directory"
        );
        assert!(!dir.join("nested/out.bin").exists());

        // Unsupported encoding into a (different) missing nested parent.
        let result = tool
            .execute(json!({
                "path": "nested2/out.txt",
                "content": "hi",
                "encoding": "hex"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Unsupported encoding")
        );
        assert!(
            !dir.join("nested2").exists(),
            "unsupported encoding must not create the parent directory"
        );
        assert!(!dir.join("nested2/out.txt").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_base64_still_blocks_path_traversal() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_base64_traversal");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"bad");
        let tool = wrapped_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "../../etc/evil", "content": encoded, "encoding": "base64"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Path blocked"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_empty_content() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_empty");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
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

        let tool = test_tool(workspace.clone());
        let result = tool
            .execute(json!({"path": "escape_dir/hijack.txt", "content": "bad"}))
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
        assert!(!outside.join("hijack.txt").exists());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_write_blocks_ephemeral_runtime() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_ephemeral");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = ephemeral_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "out.txt", "content": "should-block"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("ephemeral workspace"),
            "error should mention ephemeral workspace, got: {:?}",
            result.error
        );
        assert!(
            !dir.join("out.txt").exists(),
            "no file should be written in ephemeral mode"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_readonly_mode() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_readonly");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool_with(dir.clone(), AutonomyLevel::ReadOnly, 20);
        let result = tool
            .execute(json!({"path": "out.txt", "content": "should-block"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("read-only"));
        assert!(!dir.join("out.txt").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

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

        tokio::fs::write(outside.join("target.txt"), "original")
            .await
            .unwrap();
        symlink(outside.join("target.txt"), workspace.join("linked.txt")).unwrap();

        let tool = test_tool(workspace.clone());
        let result = tool
            .execute(json!({"path": "linked.txt", "content": "overwritten"}))
            .await
            .unwrap();

        assert!(!result.success, "writing through symlink must be blocked");
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
    async fn file_write_absolute_path_in_workspace() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_abs_path");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Canonicalize so the workspace dir matches resolved paths on macOS (/private/var/…)
        let dir = tokio::fs::canonicalize(&dir).await.unwrap();

        let tool = test_tool(dir.clone());

        let abs_path = dir.join("abs_test.txt");
        let result = tool
            .execute(
                json!({"path": abs_path.to_string_lossy().to_string(), "content": "absolute!"}),
            )
            .await
            .unwrap();

        assert!(
            result.success,
            "writing via absolute workspace path should succeed, error: {:?}",
            result.error
        );

        let content = tokio::fs::read_to_string(dir.join("abs_test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "absolute!");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_null_byte_in_path() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_write_null");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "file\u{0000}.txt", "content": "bad"}))
            .await
            .unwrap();
        assert!(!result.success, "paths with null bytes must be blocked");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_blocks_path_outside_workspace() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_write_outside_workspace");
        let workspace = root.join("workspace");
        let outside_file = root.join("outside.txt");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let tool = test_tool(workspace.clone());
        let result = tool
            .execute(json!({
                "path": outside_file.to_string_lossy(),
                "content": "should-block"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(!outside_file.exists());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
