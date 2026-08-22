use crate::helpers::filesystem_boundary::{
    FilesystemBoundaryError, create_dir_path_nofollow, open_absolute_dir_nofollow,
    write_file_atomic,
};
use async_trait::async_trait;
use cap_std::fs::Dir;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
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
                output: ToolOutput::default(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.persistent_writes {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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
                            output: ToolOutput::default(),
                            error: Some(format!("Invalid base64 content: {e}")),
                        });
                    }
                }
            }
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Unsupported encoding '{other}' (expected 'utf8' or 'base64')"
                    )),
                });
            }
        };

        // Rate limiting and path-allowlist checks are applied by the
        // RateLimitedTool + PathGuardedTool wrappers at registration time
        // (see zeroclaw-runtime::tools::mod).

        // This tool can also be constructed directly, so reject lexical
        // traversal before resolving or creating any part of the target path.
        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_text_arg(
                    "tool-file-write-error-path-blocked",
                    "path",
                    path,
                )),
            });
        }

        let full_path = self.security.resolve_tool_path(path);

        let Some(parent) = full_path.parent() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_text("tool-file-write-error-missing-parent")),
            });
        };

        // Authorize the nearest existing ancestor and the prospective parent
        // before creating anything. This prevents a denied target from leaving
        // behind attacker-chosen directories outside the workspace boundary.
        let mut existing_ancestor = parent;
        loop {
            match tokio::fs::symlink_metadata(existing_ancestor).await {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let Some(next) = existing_ancestor.parent() else {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(tool_text("tool-file-write-error-no-existing-parent")),
                        });
                    };
                    existing_ancestor = next;
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }

        let canonical_ancestor = tokio::fs::canonicalize(existing_ancestor).await?;
        let missing_suffix = match parent.strip_prefix(existing_ancestor) {
            Ok(suffix) => suffix,
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_text_arg(
                        "tool-file-write-error-parent-binding",
                        "path",
                        &parent.display().to_string(),
                    )),
                });
            }
        };
        let prospective_parent = canonical_ancestor.join(missing_suffix);
        if !self.security.is_resolved_path_allowed(&prospective_parent) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_text_arg(
                    "tool-file-write-error-path-blocked",
                    "path",
                    &prospective_parent.display().to_string(),
                )),
            });
        }

        let Some(file_name) = full_path.file_name() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_text("tool-file-write-error-missing-name")),
            });
        };
        let prospective_target = prospective_parent.join(file_name);
        if self.security.is_runtime_config_path(&full_path)
            || self.security.is_runtime_config_path(&prospective_target)
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_text_arg(
                    "tool-file-write-error-runtime-config",
                    "path",
                    &prospective_target.display().to_string(),
                )),
            });
        }

        let capability_relative = match prospective_parent.strip_prefix(&canonical_ancestor) {
            Ok(relative) => relative,
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(tool_text("tool-file-write-error-capability-binding")),
                });
            }
        };
        let capability_root = canonical_ancestor;
        let capability_relative = capability_relative.to_path_buf();
        let file_name = file_name.to_os_string();
        let display_path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            let parent_dir = match create_dir_beneath(&capability_root, &capability_relative) {
                Ok(dir) => dir,
                Err(error) => match error.downcast_ref::<FilesystemBoundaryError>() {
                    Some(boundary) if boundary.is_denied() => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(localize_filesystem_boundary(boundary)),
                        });
                    }
                    _ => return Err(error),
                },
            };

            // The returned parent handle is the bound authority. Re-resolving
            // the ambient pathname here would reintroduce a post-mutation race.
            match parent_dir.symlink_metadata(&file_name) {
                Ok(meta) if meta.is_symlink() => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(tool_text_arg(
                            "tool-file-write-error-symlink",
                            "path",
                            &prospective_target.display().to_string(),
                        )),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }

            write_file_atomic(&parent_dir, Path::new(&file_name), &bytes)?;
            Ok(ToolResult {
                success: true,
                output: format!("Written {} bytes to {display_path}", bytes.len()).into(),
                error: None,
            })
        })
        .await?
    }
}

fn create_dir_beneath(root: &Path, relative: &Path) -> anyhow::Result<Dir> {
    let root = open_absolute_dir_nofollow(root)?;
    Ok(create_dir_path_nofollow(&root, relative)?)
}

fn localize_filesystem_boundary(error: &FilesystemBoundaryError) -> String {
    match error.localization() {
        Some((key, path)) => {
            crate::i18n::get_required_tool_string_with_args(key, &[("path", &path)])
        }
        None => error.to_string(),
    }
}

fn tool_text(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

fn tool_text_arg(key: &str, name: &str, value: &str) -> String {
    crate::i18n::get_required_tool_string_with_args(key, &[(name, value)])
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
    async fn file_write_propagates_unexpected_install_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("occupied")).unwrap();
        let tool = test_tool(dir.path().to_path_buf());

        let result = tool
            .execute(json!({"path": "occupied", "content": "data"}))
            .await;

        assert!(result.is_err());
        assert!(dir.path().join("occupied").is_dir());
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
    async fn file_write_normalizes_workspace_prefixed_relative_path() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_write_workspace_prefixed");
        let workspace = root.join("workspace");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let tool = test_tool(workspace.clone());
        let workspace_prefixed =
            crate::util_helpers::workspace_prefixed_relative_path_for_test(&workspace)
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
    async fn file_write_rejects_parent_before_creating_directories() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside_parent = root.path().join("outside").join("nested");
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let tool = test_tool(workspace);
        let result = tool
            .execute(json!({
                "path": outside_parent.join("blocked.txt").to_string_lossy(),
                "content": "blocked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            !root.path().join("outside").exists(),
            "authorization must happen before parent creation"
        );
    }

    #[tokio::test]
    async fn file_write_rejects_runtime_config_before_creating_parent() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let protected_parent = workspace.join("missing-config-dir");
        let protected_path = protected_parent.join("config.toml");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            config_path: Some(protected_path.clone()),
            ..SecurityPolicy::default()
        });
        let tool = FileWriteTool::new(security);

        let result = tool
            .execute(json!({
                "path": protected_path.to_string_lossy(),
                "content": "blocked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            !protected_parent.exists(),
            "runtime-config rejection must precede parent creation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_rejects_runtime_config_through_symlinked_workspace() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let real_workspace = root.path().join("real-workspace");
        let workspace_link = root.path().join("workspace-link");
        tokio::fs::create_dir_all(&real_workspace).await.unwrap();
        symlink(&real_workspace, &workspace_link).unwrap();
        let protected_parent = workspace_link.join("missing-config-dir");
        let protected_path = protected_parent.join("config.toml");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace_link,
            config_path: Some(protected_path.clone()),
            ..SecurityPolicy::default()
        });
        let tool = FileWriteTool::new(security);

        let result = tool
            .execute(json!({
                "path": protected_path.to_string_lossy(),
                "content": "blocked"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(!real_workspace.join("missing-config-dir").exists());
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
                .contains("Path blocked by security policy")
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

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_replaces_hard_link_without_mutating_external_inode() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside.txt");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        std::fs::write(&outside, "outside").unwrap();
        std::fs::hard_link(&outside, workspace.join("linked.txt")).unwrap();
        let tool = test_tool(workspace.clone());

        let result = tool
            .execute(json!({"path": "linked.txt", "content": "workspace"}))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
        assert_eq!(
            std::fs::read_to_string(workspace.join("linked.txt")).unwrap(),
            "workspace"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_preserves_existing_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("script.sh");
        std::fs::write(&script, "old").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tool = test_tool(root.path().to_path_buf());

        let result = tool
            .execute(json!({"path": "script.sh", "content": "new"}))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(
            std::fs::metadata(script).unwrap().permissions().mode() & 0o777,
            0o755
        );
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
