use crate::helpers::filesystem_boundary::{
    FilesystemBoundaryError, create_dir_path_nofollow, open_absolute_dir_nofollow,
    open_file_nofollow, write_file_atomic,
};
use async_trait::async_trait;
use cap_std::fs::Dir;
use serde_json::json;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_api::local_file_diff::{
    MAX_FILE_DIFF_BYTES, MAX_FILE_DIFF_LINES, current_capture, is_diff_text,
};
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
        if !self.security.is_resolved_path_allowed(&prospective_target) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(tool_text_arg(
                    "tool-file-write-error-path-blocked",
                    "path",
                    &prospective_target.display().to_string(),
                )),
            });
        }
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
        // Task-local context does not propagate into spawn_blocking. Clone only
        // the invocation's admitted handle, never a long-lived capture flag.
        let local_diff_capture = current_capture().filter(|_| {
            encoding == "utf8"
                && content.len() <= MAX_FILE_DIFF_BYTES
                && content.lines().count() <= MAX_FILE_DIFF_LINES
                && is_diff_text(content)
        });
        let canonical_workspace = if local_diff_capture.is_some() {
            tokio::fs::canonicalize(&self.security.workspace_dir)
                .await
                .ok()
        } else {
            None
        };
        let security = self.security.clone();
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
            // Ordinary output retains only this pre-write metadata observation.
            // It is not an atomic snapshot of what a concurrent writer replaces.
            let previous_bytes = match parent_dir.symlink_metadata(&file_name) {
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
                Ok(meta) => Some(meta.len()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };

            let captured_previous = local_diff_capture.as_ref().and_then(|_| {
                let workspace = canonical_workspace.as_ref()?;
                if !prospective_target.starts_with(workspace)
                    || !security.is_resolved_path_readable(&prospective_target)
                    || excluded_diff_path(&prospective_target)
                {
                    return None;
                }
                capture_previous_text(&parent_dir, Path::new(&file_name), previous_bytes)
            });

            if let Err(error) = write_file_atomic(&parent_dir, Path::new(&file_name), &bytes) {
                if error.is_denied() {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(localize_filesystem_boundary(&error)),
                    });
                }
                return Err(error.into());
            }
            // Publish only after a successful replace. Collection failures never
            // change the authorized write or expose old text in its result.
            if let (Some(capture), Some(previous), Ok(written)) = (
                local_diff_capture,
                captured_previous,
                std::str::from_utf8(&bytes),
            ) {
                capture.record(previous, written.to_owned());
            }
            let new_bytes = bytes.len().to_string();
            let output = match previous_bytes {
                Some(previous_bytes) => crate::i18n::get_required_tool_string_with_args(
                    "tool-file-write-result-existing",
                    &[
                        ("bytes", &new_bytes),
                        ("path", &display_path),
                        ("previous_bytes", &previous_bytes.to_string()),
                    ],
                ),
                None => crate::i18n::get_required_tool_string_with_args(
                    "tool-file-write-result-absent",
                    &[("bytes", &new_bytes), ("path", &display_path)],
                ),
            };
            Ok(ToolResult {
                success: true,
                output: output.into(),
                error: None,
            })
        })
        .await?
    }
}

fn excluded_diff_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        ".env" | ".secret_key" | "credentials.json" | "auth.json" | "id_rsa" | "id_ed25519"
    ) || name.starts_with(".env.")
        || matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("pem" | "key" | "p12" | "pfx")
        )
}

fn capture_previous_text(parent: &Dir, leaf: &Path, previous_bytes: Option<u64>) -> Option<String> {
    let Some(previous_bytes) = previous_bytes else {
        return Some(String::new());
    };
    if previous_bytes > MAX_FILE_DIFF_BYTES as u64 {
        return None;
    }
    let file = open_file_nofollow(parent, leaf).ok()?;
    let metadata = file.metadata().ok()?;
    if metadata.len() > MAX_FILE_DIFF_BYTES as u64 {
        return None;
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return None;
        }
    }
    let mut bytes = Vec::new();
    file.take((MAX_FILE_DIFF_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_FILE_DIFF_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.lines().count() > MAX_FILE_DIFF_LINES || !is_diff_text(&text) {
        return None;
    }
    Some(text)
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
    use zeroclaw_api::local_file_diff::{
        LOCAL_FILE_DIFF_CAPTURE, LocalFileDiff, LocalFileDiffCapture,
    };
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

    async fn execute_with_diff_capture(
        tool: &FileWriteTool,
        args: serde_json::Value,
    ) -> (anyhow::Result<ToolResult>, Option<LocalFileDiff>) {
        let capture = LocalFileDiffCapture::new();
        let result = LOCAL_FILE_DIFF_CAPTURE
            .scope(Some(capture.clone()), tool.execute(args))
            .await;
        (result, capture.take())
    }

    #[tokio::test]
    async fn file_write_local_diff_keeps_previous_text_out_of_result() {
        let root = tempfile::tempdir().unwrap();
        let previous = "private-removed-sentinel\nunchanged\n";
        let written = "replacement\nunchanged\n";
        std::fs::write(root.path().join("notes.txt"), previous).unwrap();
        let tool = test_tool(root.path().to_path_buf());

        let (result, diff) =
            execute_with_diff_capture(&tool, json!({"path": "notes.txt", "content": written}))
                .await;
        let result = result.unwrap();
        assert!(result.success, "error: {:?}", result.error);
        let diff = diff.unwrap();
        assert_eq!(diff.previous(), previous);
        assert_eq!(diff.written(), written);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private-removed-sentinel")
        );
        assert!(!format!("{result:?}").contains("private-removed-sentinel"));
        assert!(!format!("{diff:?}").contains("private-removed-sentinel"));
        assert!(
            result
                .output
                .contains("Previous contents are omitted from this result.")
        );
    }

    #[tokio::test]
    async fn file_write_local_diff_distinguishes_absent_and_empty_files() {
        let root = tempfile::tempdir().unwrap();
        let tool = test_tool(root.path().to_path_buf());
        std::fs::write(root.path().join("empty.txt"), []).unwrap();

        for (path, expected) in [
            ("nested/new.txt", "Before write: file absent."),
            ("empty.txt", "Before write: existing file, 0 bytes."),
        ] {
            let (result, diff) =
                execute_with_diff_capture(&tool, json!({"path": path, "content": "new"})).await;
            let result = result.unwrap();
            assert!(result.success, "error: {:?}", result.error);
            assert!(result.output.contains(expected));
            let diff = diff.unwrap();
            assert_eq!(diff.previous(), "");
            assert_eq!(diff.written(), "new");
        }
    }

    #[tokio::test]
    async fn file_write_local_diff_requires_invocation_context() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("notes.txt"), "private-old-sentinel").unwrap();
        let tool = test_tool(root.path().to_path_buf());
        assert!(current_capture().is_none());
        let result = tool
            .execute(json!({"path": "notes.txt", "content": "first"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private-old-sentinel")
        );

        // An explicitly disabled nested invocation must not use an outer sink.
        let capture = LocalFileDiffCapture::new();
        let result = LOCAL_FILE_DIFF_CAPTURE
            .scope(
                Some(capture.clone()),
                LOCAL_FILE_DIFF_CAPTURE.scope(
                    None,
                    tool.execute(json!({"path": "notes.txt", "content": "second"})),
                ),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(capture.take().is_none());
    }

    #[tokio::test]
    async fn file_write_local_diff_skips_foreign_readable_and_write_only_roots() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let foreign = root.path().join("foreign");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let foreign = foreign.canonicalize().unwrap();
        let target = foreign.join("notes.txt");

        for read_allowed in [true, false] {
            std::fs::write(&target, "private-foreign-sentinel").unwrap();
            let security = Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Supervised,
                workspace_dir: workspace.clone(),
                allowed_roots: if read_allowed {
                    vec![foreign.clone()]
                } else {
                    vec![]
                },
                allowed_roots_write_only: if read_allowed {
                    vec![]
                } else {
                    vec![foreign.clone()]
                },
                ..SecurityPolicy::default()
            });
            assert_eq!(security.is_resolved_path_readable(&target), read_allowed);
            let tool = FileWriteTool::new(security);
            let (result, diff) = execute_with_diff_capture(
                &tool,
                json!({"path": target.to_string_lossy(), "content": "replacement"}),
            )
            .await;
            let result = result.unwrap();
            assert!(result.success, "error: {:?}", result.error);
            assert!(diff.is_none());
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("private-foreign-sentinel")
            );
        }
    }

    #[tokio::test]
    async fn file_write_local_diff_skips_binary_large_and_control_text() {
        let root = tempfile::tempdir().unwrap();
        let tool = test_tool(root.path().to_path_buf());
        let cases = [
            (vec![0xff, 0xfe], "new".to_owned()),
            (b"old\0value".to_vec(), "new".to_owned()),
            (b"old\x1bvalue".to_vec(), "new".to_owned()),
            (vec![b'x'; MAX_FILE_DIFF_BYTES + 1], "new".to_owned()),
            (
                "x\n".repeat(MAX_FILE_DIFF_LINES + 1).into_bytes(),
                "new".to_owned(),
            ),
            (b"old".to_vec(), "new\0value".to_owned()),
            (b"old".to_vec(), "new\u{0085}value".to_owned()),
            (b"old".to_vec(), "x".repeat(MAX_FILE_DIFF_BYTES + 1)),
            (b"old".to_vec(), "x\n".repeat(MAX_FILE_DIFF_LINES + 1)),
        ];
        for (previous, written) in cases {
            std::fs::write(root.path().join("notes.txt"), previous).unwrap();
            let (result, diff) =
                execute_with_diff_capture(&tool, json!({"path": "notes.txt", "content": written}))
                    .await;
            let result = result.unwrap();
            assert!(result.success, "error: {:?}", result.error);
            assert!(diff.is_none());
            assert_eq!(
                std::fs::read_to_string(root.path().join("notes.txt")).unwrap(),
                written
            );
        }

        use base64::Engine;
        std::fs::write(root.path().join("notes.txt"), "old").unwrap();
        let (result, diff) = execute_with_diff_capture(
            &tool,
            json!({
                "path": "notes.txt", "content": base64::engine::general_purpose::STANDARD.encode(b"new"),
                "encoding": "base64"
            }),
        )
        .await;
        assert!(result.unwrap().success);
        assert!(
            diff.is_none(),
            "even textual base64 writes have no local diff"
        );
    }

    #[tokio::test]
    async fn file_write_local_diff_skips_credential_and_key_files() {
        let root = tempfile::tempdir().unwrap();
        let tool = test_tool(root.path().to_path_buf());
        for path in [
            ".env",
            ".env.local",
            ".ENV.production",
            ".secret_key",
            "credentials.json",
            "auth.json",
            "id_rsa",
            "id_ed25519",
            "cert.pem",
            "private.key",
            "identity.p12",
            "identity.pfx",
            "CERT.PEM",
        ] {
            std::fs::write(root.path().join(path), "private-key-sentinel").unwrap();
            let (result, diff) =
                execute_with_diff_capture(&tool, json!({"path": path, "content": "replacement"}))
                    .await;
            let result = result.unwrap();
            assert!(result.success, "{path}: {:?}", result.error);
            assert!(diff.is_none(), "{path} must not retain previous contents");
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("private-key-sentinel")
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_local_diff_skips_hardlinked_contents() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, "private-linked-sentinel").unwrap();
        std::fs::hard_link(&outside, workspace.join("linked.txt")).unwrap();
        let tool = test_tool(workspace.clone());
        let (result, diff) = execute_with_diff_capture(
            &tool,
            json!({"path": "linked.txt", "content": "replacement"}),
        )
        .await;
        assert!(result.unwrap().success);
        assert!(diff.is_none());
        assert_eq!(
            std::fs::read_to_string(outside).unwrap(),
            "private-linked-sentinel"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("linked.txt")).unwrap(),
            "replacement"
        );
    }

    #[tokio::test]
    async fn file_write_local_diff_failure_has_no_evidence() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("occupied")).unwrap();
        let tool = test_tool(root.path().to_path_buf());
        let (result, diff) =
            execute_with_diff_capture(&tool, json!({"path": "occupied", "content": "replacement"}))
                .await;
        assert!(!result.unwrap().success);
        assert!(diff.is_none());
        assert!(root.path().join("occupied").is_dir());
    }

    #[test]
    fn file_write_local_diff_old_read_failure_skips_capture() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_absolute_dir_nofollow(&root.path().canonicalize().unwrap()).unwrap();
        assert!(capture_previous_text(&parent, Path::new("missing.txt"), Some(12)).is_none());
    }

    #[cfg(target_os = "windows")]
    fn absolute_path_outside_workspace() -> &'static str {
        r"C:\Windows\win.ini"
    }

    #[cfg(not(target_os = "windows"))]
    fn absolute_path_outside_workspace() -> &'static str {
        "/etc/evil"
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_write_preserves_special_file_destination() {
        use std::os::unix::fs::FileTypeExt;

        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.path().join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        let result = wrapped_tool(root.path().to_path_buf())
            .execute(json!({"path": "pipe", "content": "replacement"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some(
                tool_text_arg("tool-filesystem-boundary-error-not-regular", "path", "pipe")
                    .as_str()
            )
        );
        assert!(
            std::fs::symlink_metadata(root.path().join("pipe"))
                .unwrap()
                .file_type()
                .is_fifo()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
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
        assert!(result.output.contains("Before write: file absent."));
        assert!(
            result
                .output
                .contains("Previous contents are omitted from this result.")
        );

        let content = tokio::fs::read_to_string(dir.join("out.txt"))
            .await
            .unwrap();
        assert_eq!(content, "written!");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_write_rejects_directory_destination() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("occupied")).unwrap();
        let tool = test_tool(dir.path().to_path_buf());

        let result = tool
            .execute(json!({"path": "occupied", "content": "data"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some(
                tool_text_arg(
                    "tool-filesystem-boundary-error-not-regular",
                    "path",
                    "occupied"
                )
                .as_str()
            )
        );
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
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        // This unlabelled value would evade credential-pattern redaction.
        let previous = "private-deleted-value";
        tokio::fs::write(dir.join("exist.txt"), previous)
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "exist.txt", "content": "new"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Written 3 bytes"));
        assert!(
            result
                .output
                .contains(&format!("existing file, {} bytes", previous.len()))
        );
        assert!(
            result
                .output
                .contains("Previous contents are omitted from this result.")
        );
        assert!(!serde_json::to_string(&result).unwrap().contains(previous));

        let content = tokio::fs::read_to_string(dir.join("exist.txt"))
            .await
            .unwrap();
        assert_eq!(content, "new");

        // Later mutation cannot rewrite the already returned history evidence.
        tokio::fs::write(dir.join("exist.txt"), "later content")
            .await
            .unwrap();
        assert!(
            result
                .output
                .contains(&format!("existing file, {} bytes", previous.len()))
        );
    }

    #[tokio::test]
    async fn file_write_empty_existing_file_is_not_absent() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("empty.bin"), []).unwrap();
        let result = test_tool(temp.path().to_path_buf())
            .execute(json!({"path": "empty.bin", "content": "AAEC", "encoding": "base64"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Written 3 bytes"));
        assert!(result.output.contains("existing file, 0 bytes"));
        assert!(!result.output.contains("file absent"));
        assert_eq!(
            std::fs::read(temp.path().join("empty.bin")).unwrap(),
            [0, 1, 2]
        );
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
    async fn file_write_rejects_exact_forbidden_target_through_production_wrappers() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let target = workspace.join("blocked.txt");
        tokio::fs::write(&target, "original").await.unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            forbidden_paths: vec![target.to_string_lossy().into_owned()],
            ..SecurityPolicy::default()
        });
        let tool = RateLimitedTool::new(
            PathGuardedTool::new(FileWriteTool::new(security.clone()), security.clone()),
            security,
        );

        let result = tool
            .execute(json!({"path": "blocked.txt", "content": "replaced"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "original"
        );

        tokio::fs::remove_file(&target).await.unwrap();
        let result = tool
            .execute(json!({"path": "blocked.txt", "content": "created"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(!target.exists());
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
