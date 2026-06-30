use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult, with_ephemeral_workspace_warning};

const MAX_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;

/// Read file contents with workspace sandboxing.
pub struct FileReadTool {
    security: Arc<SecurityPolicy>,
    /// Whether the workspace is host-persistent. `false` on an ephemeral
    /// runtime (Docker tmpfs / no volume mount), where reads can return stale
    /// or empty data that does not reflect the host filesystem. When `false`,
    /// successful text reads carry a loud ephemeral-workspace warning so the
    /// agent doesn't trust the contents as host-backed. See issue #4627.
    persistent_writes: bool,
}

impl FileReadTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            persistent_writes: true,
        }
    }

    /// Construct with an explicit persistence flag derived from the active
    /// runtime adapter's `has_filesystem_access()`. Mirrors
    /// [`super::FileWriteTool::new_with_persistence`].
    pub fn new_with_persistence(security: Arc<SecurityPolicy>, persistent_writes: bool) -> Self {
        Self {
            security,
            persistent_writes,
        }
    }

    /// Resolve a caller-supplied path to an absolute candidate. Reject
    /// only path-shape attacks (null byte, `..` traversal); the
    /// allowlist gate is `SecurityPolicy::is_resolved_path_readable`
    /// after canonicalize, which already unions `allowed_roots` and
    /// `allowed_roots_read_only`.
    fn resolve_candidate(&self, path: &str) -> anyhow::Result<std::path::PathBuf> {
        if path.contains('\0') {
            anyhow::bail!("Path not allowed: contains null byte");
        }
        if std::path::Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            anyhow::bail!("Path not allowed by security policy: {path}");
        }

        Ok(self.security.resolve_tool_path(path))
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read file contents with line numbers. Supports partial reading via offset and limit. Extracts text from PDF; binary and image files are rejected (use the image_info tool for images). Set encoding=\"base64\" to return raw bytes base64-encoded (for binary files such as .xlsx/.docx); offset/limit are ignored in that mode."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Relative paths resolve from workspace root; absolute paths must be within the workspace."
                },
                "offset": {
                    "type": "integer",
                    "description": "Starting line number (1-based, default: 1). Ignored when encoding is 'base64'."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: all). Ignored when encoding is 'base64'."
                },
                "encoding": {
                    "type": "string",
                    "enum": ["utf8", "base64"],
                    "description": "Output encoding (default: 'utf8'). Use 'base64' to read binary files as base64-encoded bytes."
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Base64 reads return a verbatim payload the caller decodes, so they
        // must NOT be annotated — a prepended banner would corrupt decoding.
        // Text reads on an ephemeral runtime may return stale/empty data, so
        // they carry the loud warning instead (issue #4627).
        let is_base64 = args.get("encoding").and_then(|v| v.as_str()) == Some("base64");
        let mut result = self.read_path(args).await?;
        if !self.persistent_writes && result.success && !is_base64 {
            result.output = with_ephemeral_workspace_warning(&result.output);
        }
        Ok(result)
    }
}

impl FileReadTool {
    /// Resolve, sandbox-check, and read the requested path. The ephemeral
    /// workspace warning is applied by the `Tool::execute` wrapper above.
    async fn read_path(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "path"})),
                "tool argument validation failed"
            );

            anyhow::Error::msg("Missing 'path' parameter")
        })?;

        // Cross-cutting rate limiting and path-allowlist checks live in the
        // RateLimitedTool + PathGuardedTool wrappers at registration time
        // (see zeroclaw-runtime::tools::mod).  Successful reads consume one
        // budget slot via the outer RateLimitedTool.
        //
        // Read-tool exception: post-`PathGuardedTool` resolve/canonicalize
        // failures (path-traversal that slipped through allowlist, missing
        // file) also consume one budget slot, charged here, so that callers
        // cannot probe path existence for free.  The outer wrapper only
        // records on `success: true`, so calling `record_action()` on these
        // failure paths charges exactly one slot per attempt — matching the
        // pre-wrapper semantics where every attempted read cost one slot.

        // Validate and build candidate path using workspace_dir directly.
        let full_path = match self.resolve_candidate(path) {
            Ok(p) => p,
            Err(e) => {
                let _ = self.security.record_action();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        // Canonicalize to resolve symlinks, then enforce workspace boundary.
        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                let _ = self.security.record_action();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        // Read access: workspace + read-write allowlist + read-only allowlist
        // + universal POSIX device files (/dev/null, etc.).
        if !self.security.is_resolved_path_readable(&resolved_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path escapes workspace directory: {path}")),
            });
        }

        // Check file size AFTER canonicalization to prevent TOCTOU symlink bypass
        match tokio::fs::metadata(&resolved_path).await {
            Ok(meta) => {
                if meta.len() > MAX_FILE_SIZE_BYTES {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "File too large: {} bytes (limit: {MAX_FILE_SIZE_BYTES} bytes)",
                            meta.len()
                        )),
                    });
                }
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file metadata: {e}")),
                });
            }
        }

        let encoding = args
            .get("encoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf8");

        if encoding == "base64" {
            // Binary read: return raw bytes base64-encoded. Line numbering and
            // offset/limit are text concepts and do not apply here.
            let bytes = match tokio::fs::read(&resolved_path).await {
                Ok(b) => b,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read file: {e}")),
                    });
                }
            };
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Ok(ToolResult {
                success: true,
                output: encoded,
                error: None,
            });
        } else if encoding != "utf8" {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported encoding '{encoding}' (expected 'utf8' or 'base64')"
                )),
            });
        }

        match tokio::fs::read_to_string(&resolved_path).await {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                let total = lines.len();

                if total == 0 {
                    return Ok(ToolResult {
                        success: true,
                        output: String::new(),
                        error: None,
                    });
                }

                let offset = args
                    .get("offset")
                    .and_then(|v| v.as_u64())
                    .map(|v| {
                        usize::try_from(v.max(1))
                            .unwrap_or(usize::MAX)
                            .saturating_sub(1)
                    })
                    .unwrap_or(0);
                let start = offset.min(total);

                let end = match args.get("limit").and_then(|v| v.as_u64()) {
                    Some(l) => {
                        let limit = usize::try_from(l).unwrap_or(usize::MAX);
                        (start.saturating_add(limit)).min(total)
                    }
                    None => total,
                };

                if start >= end {
                    return Ok(ToolResult {
                        success: true,
                        output: format!("[No lines in range, file has {total} lines]"),
                        error: None,
                    });
                }

                let numbered: String = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{}: {}", start + i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");

                let partial = start > 0 || end < total;
                let summary = if partial {
                    format!("\n[Lines {}-{} of {total}]", start + 1, end)
                } else {
                    format!("\n[{total} lines total]")
                };

                Ok(ToolResult {
                    success: true,
                    output: format!("{numbered}{summary}"),
                    error: None,
                })
            }
            Err(_) => {
                // Not valid UTF-8 — read raw bytes and try to extract text
                let bytes = tokio::fs::read(&resolved_path).await.map_err(|e| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "path": resolved_path.display().to_string(),
                                "error": format!("{}", e),
                            })),
                        "file_read: raw byte fallback read failed"
                    );
                    anyhow::Error::msg(format!("Failed to read file: {e}"))
                })?;

                if let Some(text) = try_extract_pdf_text(&bytes) {
                    return Ok(ToolResult {
                        success: true,
                        output: text,
                        error: None,
                    });
                }

                // Reject confident binary instead of returning lossy garbage.
                // Known image formats: point the agent at the image_info tool.
                if let Some(kind) = detect_image_format(&bytes) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Binary image file detected ({kind}): {}. Use the image_info \
                             tool for images, or encoding=\"base64\" to read the raw bytes.",
                            resolved_path.display()
                        )),
                    });
                }

                // Other confident binary (NUL byte or a glut of control bytes).
                if looks_binary(&bytes) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Binary file detected: {}. Use encoding=\"base64\" to read the \
                             raw bytes.",
                            resolved_path.display()
                        )),
                    });
                }

                // Not confidently binary — most likely text in a non-UTF-8 encoding
                // (e.g. Windows-1251, Latin-1). Decode leniently for now; proper
                // charset detection/transcoding is tracked as a follow-up.
                let lossy = String::from_utf8_lossy(&bytes).into_owned();
                Ok(ToolResult {
                    success: true,
                    output: lossy,
                    error: None,
                })
            }
        }
    }
}

#[cfg(feature = "rag-pdf")]
fn try_extract_pdf_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 5 || &bytes[..5] != b"%PDF-" {
        return None;
    }
    let text = pdf_extract::extract_text_from_mem(bytes).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

#[cfg(not(feature = "rag-pdf"))]
fn try_extract_pdf_text(_bytes: &[u8]) -> Option<String> {
    None
}

/// Detect a common raster-image container by its file-header magic bytes.
/// Returns the format name when recognized so `file_read` can reject images
/// with guidance to use the `image_info` tool instead of emitting lossy text.
/// Only consulted on the non-UTF-8 read path, so an ASCII string that merely
/// starts with one of these markers (and is therefore valid UTF-8) is unaffected.
///
/// PNG/JPEG/GIF magics carry non-ASCII/control bytes and are collision-free, and
/// WEBP is anchored by the `RIFF…WEBP` container, so the raw magic is enough. The
/// BMP marker is just the two printable ASCII letters `BM`, which a non-UTF-8
/// legacy-text file can legitimately start with (a name, "BMW dealer notes", …),
/// so it is validated against the rest of the BITMAPFILEHEADER instead of trusted
/// on the magic alone — otherwise the legacy-text carve-out below is defeated.
fn detect_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if is_bmp_header(bytes) {
        Some("bmp")
    } else {
        None
    }
}

/// Validate a BMP `BITMAPFILEHEADER` beyond the weak `BM` magic. Real BMPs set
/// the two reserved words (offset 6..10) to zero and point `bfOffBits`
/// (offset 10..14, the pixel-array offset) inside the file. Non-UTF-8 text that
/// merely starts with `BM` carries printable bytes in the reserved field, so it
/// fails this check and falls through to the lenient lossy read. `bfSize`
/// (offset 2..6) is deliberately not checked: some encoders write 0 there.
fn is_bmp_header(bytes: &[u8]) -> bool {
    if bytes.len() < 14 || !bytes.starts_with(b"BM") {
        return false;
    }
    if bytes[6..10] != [0, 0, 0, 0] {
        return false;
    }
    let off_bits = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
    (14..=bytes.len() as u32).contains(&off_bits)
}

/// Heuristic binary classifier for the non-UTF-8 read path. A NUL byte (which
/// text essentially never contains) or a high density of non-text control
/// characters marks the content as binary. Legacy single-byte text encodings
/// (e.g. cp1251, Latin-1) have neither, so they are deliberately NOT classified
/// as binary here — they fall through to the lenient lossy read.
fn looks_binary(bytes: &[u8]) -> bool {
    // Sample a prefix so very large files stay cheap.
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.is_empty() {
        return false;
    }
    if sample.contains(&0) {
        return true;
    }
    let is_control = |b: u8| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r';
    let controls = sample.iter().filter(|&&b| is_control(b)).count();
    controls * 100 / sample.len() > 30
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool(workspace: std::path::PathBuf) -> FileReadTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        FileReadTool::new(security)
    }

    fn test_tool_with(
        workspace: std::path::PathBuf,
        autonomy: AutonomyLevel,
        max_actions_per_hour: u32,
    ) -> FileReadTool {
        let security = Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: workspace,
            max_actions_per_hour,
            ..SecurityPolicy::default()
        });
        FileReadTool::new(security)
    }

    fn ephemeral_tool(workspace: std::path::PathBuf) -> FileReadTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        FileReadTool::new_with_persistence(security, false)
    }

    fn workspace_prefixed_relative_path_for_test(
        workspace: &std::path::Path,
    ) -> std::path::PathBuf {
        let mut relative = std::path::PathBuf::new();
        for component in workspace.components() {
            match component {
                std::path::Component::Prefix(_)
                | std::path::Component::RootDir
                | std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    panic!("test workspace path must not contain parent components")
                }
                std::path::Component::Normal(part) => relative.push(part),
            }
        }
        relative
    }

    #[test]
    fn file_read_name() {
        let tool = test_tool(std::env::temp_dir());
        assert_eq!(tool.name(), "file_read");
    }

    #[test]
    fn file_read_schema_has_path() {
        let tool = test_tool(std::env::temp_dir());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["offset"].is_object());
        assert!(schema["properties"]["limit"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("path"))
        );
        // offset and limit are optional
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("offset"))
        );
    }

    #[tokio::test]
    async fn file_read_existing_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "test.txt"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("1: hello world"));
        assert!(result.output.contains("[1 lines total]"));
        assert!(result.error.is_none());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_nonexistent_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_missing");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "nope.txt"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Failed to resolve"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_blocks_path_traversal() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_traversal");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "../../../etc/passwd"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_blocks_absolute_path() {
        let tool = test_tool(std::env::temp_dir());

        #[cfg(unix)]
        let target = "/etc/passwd";
        #[cfg(windows)]
        let target = {
            let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            std::path::PathBuf::from(sysroot).join(r"System32\drivers\etc\hosts")
        };

        let result = tool.execute(json!({"path": target})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("escapes workspace"));
    }

    #[tokio::test]
    async fn file_read_allows_readonly_mode() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_readonly");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("test.txt"), "readonly ok")
            .await
            .unwrap();

        let tool = test_tool_with(dir.clone(), AutonomyLevel::ReadOnly, 20);
        let result = tool.execute(json!({"path": "test.txt"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("1: readonly ok"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_missing_path_param() {
        let tool = test_tool(std::env::temp_dir());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn file_read_schema_has_encoding() {
        let tool = test_tool(std::env::temp_dir());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["encoding"].is_object());
    }

    #[tokio::test]
    async fn file_read_base64_returns_encoded_bytes() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_base64");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Non-UTF-8 bytes — proves we return raw bytes, not lossy text.
        let raw: Vec<u8> = vec![0x00, 0x80, 0xFF, 0xFE, b'P', b'K', 0x03, 0x04];
        tokio::fs::write(dir.join("data.bin"), &raw).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "data.bin", "encoding": "base64"}))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);

        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(result.output.trim())
            .expect("output must be valid base64");
        assert_eq!(decoded, raw, "base64 read must round-trip exact bytes");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── Ephemeral-workspace warning (issue #4627) ────────────────

    /// On an ephemeral runtime a successful text read may reflect stale/empty
    /// data; the output carries a loud warning while preserving the contents.
    #[tokio::test]
    async fn file_read_warns_on_ephemeral_workspace() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_ephemeral");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("notes.txt"), "host content?")
            .await
            .unwrap();

        let tool = ephemeral_tool(dir.clone());
        let result = tool.execute(json!({"path": "notes.txt"})).await.unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must be present, got: {}",
            result.output
        );
        assert!(result.output.contains("mount_workspace"));
        assert!(
            result.output.contains("host content?"),
            "original read content must be preserved, got: {}",
            result.output
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// base64 reads return a verbatim payload the caller decodes; prepending a
    /// banner would corrupt decoding, so base64 reads must stay un-annotated.
    #[tokio::test]
    async fn file_read_base64_not_warned_on_ephemeral_workspace() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_ephemeral_b64");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let raw: Vec<u8> = vec![0x00, 0x80, 0xFF, 0xFE, b'P', b'K'];
        tokio::fs::write(dir.join("data.bin"), &raw).await.unwrap();

        let tool = ephemeral_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "data.bin", "encoding": "base64"}))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(
            !result.output.contains("EPHEMERAL WORKSPACE"),
            "base64 payload must not be annotated, got: {}",
            result.output
        );
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(result.output.trim())
            .expect("base64 output must still decode");
        assert_eq!(decoded, raw, "base64 read must round-trip exact bytes");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A failed read returns no file data — not data loss — so no banner is
    /// attached to either field.
    #[tokio::test]
    async fn file_read_failure_not_warned_on_ephemeral_workspace() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_ephemeral_fail");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = ephemeral_tool(dir.clone());
        let result = tool.execute(json!({"path": "missing.txt"})).await.unwrap();
        assert!(!result.success);
        assert!(!result.output.contains("EPHEMERAL WORKSPACE"));
        assert!(
            !result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("EPHEMERAL WORKSPACE")
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// On a persistent runtime (the default) no warning is attached.
    #[tokio::test]
    async fn file_read_no_warning_when_persistent() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_persistent");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("notes.txt"), "ok").await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "notes.txt"})).await.unwrap();
        assert!(result.success);
        assert!(
            !result.output.contains("EPHEMERAL WORKSPACE"),
            "no ephemeral warning expected on a persistent runtime, got: {}",
            result.output
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_unsupported_encoding_errors() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_bad_encoding");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("f.txt"), "hi").await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "f.txt", "encoding": "hex"}))
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
    async fn file_read_empty_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_empty");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("empty.txt"), "").await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "empty.txt"})).await.unwrap();
        assert!(result.success);
        assert_eq!(result.output, "");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_nested_path() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_nested");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(dir.join("sub/dir"))
            .await
            .unwrap();
        tokio::fs::write(dir.join("sub/dir/deep.txt"), "deep content")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "sub/dir/deep.txt"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("1: deep content"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_normalizes_workspace_prefixed_relative_path() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_read_workspace_prefixed");
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&nested).await.unwrap();
        tokio::fs::write(nested.join("notes.txt"), "prefixed content")
            .await
            .unwrap();

        let tool = test_tool(workspace.clone());
        let workspace_prefixed =
            workspace_prefixed_relative_path_for_test(&workspace).join("nested/notes.txt");
        let result = tool
            .execute(json!({"path": workspace_prefixed.to_string_lossy()}))
            .await
            .unwrap();

        assert!(
            result.success,
            "workspace-prefixed file_read path should resolve, error: {:?}",
            result.error
        );
        assert!(result.output.contains("1: prefixed content"));

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_read_blocks_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join("zeroclaw_test_file_read_symlink_escape");
        let workspace = root.join("workspace");
        let outside = root.join("outside");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        tokio::fs::write(outside.join("secret.txt"), "outside workspace")
            .await
            .unwrap();

        symlink(outside.join("secret.txt"), workspace.join("escape.txt")).unwrap();

        let tool = test_tool(workspace.clone());
        let result = tool.execute(json!({"path": "escape.txt"})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("escapes workspace")
        );

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_read_blocks_outside_workspace_regardless_of_policy() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_read_blocks_outside");
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        let outside_file = outside.join("notes.txt");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(&outside_file, "outside").await.unwrap();

        let tool = test_tool(workspace.clone());

        let result = tool
            .execute(json!({"path": outside_file.to_string_lossy().to_string()}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("escapes workspace"));

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_read_admits_absolute_path_under_read_only_root() {
        let root =
            std::env::temp_dir().join("zeroclaw_test_file_read_admits_absolute_path_under_ro_root");
        let workspace = root.join("workspace");
        let ro_root = root.join("shared");
        let ro_file = ro_root.join("notes.txt");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&ro_root).await.unwrap();
        tokio::fs::write(&ro_file, "cross-agent read")
            .await
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            allowed_roots_read_only: vec![ro_root.clone()],
            ..SecurityPolicy::default()
        });
        let tool = FileReadTool::new(security);

        let result = tool
            .execute(json!({"path": ro_file.to_string_lossy().to_string()}))
            .await
            .unwrap();

        assert!(
            result.success,
            "absolute path under read-only root must read: {result:?}"
        );
        assert!(result.output.contains("cross-agent read"));

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn file_read_with_offset_and_limit() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_offset");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("lines.txt"), "aaa\nbbb\nccc\nddd\neee")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());

        // Read lines 2-3
        let result = tool
            .execute(json!({"path": "lines.txt", "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("2: bbb"));
        assert!(result.output.contains("3: ccc"));
        assert!(!result.output.contains("1: aaa"));
        assert!(!result.output.contains("4: ddd"));
        assert!(result.output.contains("[Lines 2-3 of 5]"));

        // Read from offset 4 to end
        let result = tool
            .execute(json!({"path": "lines.txt", "offset": 4}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("4: ddd"));
        assert!(result.output.contains("5: eee"));
        assert!(result.output.contains("[Lines 4-5 of 5]"));

        // Limit only (first 2 lines)
        let result = tool
            .execute(json!({"path": "lines.txt", "limit": 2}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("1: aaa"));
        assert!(result.output.contains("2: bbb"));
        assert!(!result.output.contains("3: ccc"));
        assert!(result.output.contains("[Lines 1-2 of 5]"));

        // Full read (no offset/limit) shows all lines
        let result = tool.execute(json!({"path": "lines.txt"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("1: aaa"));
        assert!(result.output.contains("5: eee"));
        assert!(result.output.contains("[5 lines total]"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_offset_beyond_end() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_offset_end");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("short.txt"), "one\ntwo")
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "short.txt", "offset": 100}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result
                .output
                .contains("[No lines in range, file has 2 lines]")
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_rejects_oversized_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_large");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Create a file just over 10 MB
        let big = vec![b'x'; 10 * 1024 * 1024 + 1];
        tokio::fs::write(dir.join("huge.bin"), &big).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "huge.bin"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("File too large"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// PDF files should be readable via pdf-extract text extraction.
    #[tokio::test]
    async fn file_read_extracts_pdf_text() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_pdf");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/test_document.pdf");
        tokio::fs::copy(&fixture, dir.join("report.pdf"))
            .await
            .expect("copy PDF fixture");

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "report.pdf"})).await.unwrap();

        assert!(
            result.success,
            "PDF read must succeed, error: {:?}",
            result.error
        );
        assert!(
            result.output.contains("Hello"),
            "extracted text must contain 'Hello', got: {}",
            result.output
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Confident binary (NUL byte) is rejected, not returned as lossy text.
    #[tokio::test]
    async fn file_read_rejects_binary_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_reject_binary");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Non-UTF-8 bytes containing a NUL — the classic binary signal.
        let binary_data: Vec<u8> = vec![0x00, 0x80, 0xFF, 0xFE, b'h', b'i', 0x80];
        tokio::fs::write(dir.join("data.bin"), &binary_data)
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "data.bin"})).await.unwrap();

        assert!(
            !result.success,
            "binary read must fail, got output: {:?}",
            result.output
        );
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Binary file detected"),
            "error must indicate binary rejection, got: {:?}",
            result.error
        );
        assert!(
            !result.output.contains('\u{FFFD}'),
            "must not return lossy replacement output, got: {:?}",
            result.output
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// PNG images are rejected with guidance toward the image_info tool.
    #[tokio::test]
    async fn file_read_rejects_png_image() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_reject_png");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // PNG magic (0x89 makes it invalid UTF-8) + a few header bytes.
        let png: Vec<u8> = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
        ];
        tokio::fs::write(dir.join("pic.png"), &png).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "pic.png"})).await.unwrap();

        assert!(
            !result.success,
            "image read must fail, got output: {:?}",
            result.output
        );
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            err.contains("image") && err.contains("image_info"),
            "error must point at image_info, got: {:?}",
            result.error
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// JPEG images are rejected too.
    #[tokio::test]
    async fn file_read_rejects_jpeg_image() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_reject_jpeg");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let jpeg: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
        tokio::fs::write(dir.join("pic.jpg"), &jpeg).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "pic.jpg"})).await.unwrap();

        assert!(!result.success, "jpeg read must fail");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("image"),
            "error must indicate image rejection, got: {:?}",
            result.error
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// A real BMP (valid BITMAPFILEHEADER, not just the `BM` magic) is still
    /// rejected as an image and steered to `image_info`.
    #[tokio::test]
    async fn file_read_rejects_bmp_image() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_reject_bmp");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // BMP: "BM", bfSize, reserved=0, bfOffBits=54, a 40-byte DIB header,
        // then pixel bytes. The 0xFF pixel byte makes the file invalid UTF-8 (a
        // header of only sub-0x80 bytes would be valid UTF-8 and take the fast
        // path), so it reaches the non-UTF-8 branch where detect_image_format runs.
        let mut bmp: Vec<u8> = vec![
            b'B', b'M', // magic
            0x3A, 0x00, 0x00, 0x00, // bfSize = 58
            0x00, 0x00, 0x00, 0x00, // reserved
            0x36, 0x00, 0x00, 0x00, // bfOffBits = 54
        ];
        bmp.resize(54, 0); // DIB header
        bmp.extend_from_slice(&[0xFF, 0x00, 0xFF, 0x00]); // pixel array
        tokio::fs::write(dir.join("pic.bmp"), &bmp).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "pic.bmp"})).await.unwrap();

        assert!(!result.success, "bmp read must fail");
        let err = result.error.as_deref().unwrap_or_default();
        assert!(
            err.contains("image") && err.contains("image_info"),
            "error must point at image_info, got: {:?}",
            result.error
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Non-UTF-8 legacy text that happens to start with the `BM` letters must
    /// NOT be misread as a BMP image: the reserved-field validation in
    /// `is_bmp_header` rejects it, so it falls through to the lenient lossy read.
    /// Regression for the false positive the bare `BM` magic reintroduced.
    #[tokio::test]
    async fn file_read_reads_non_utf8_bm_text_lossy() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_bm_text");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // "BM" followed by cp1251 high bytes (a Cyrillic phrase): >14 bytes, the
        // reserved field (offset 6..10) holds printable text, no NUL / control.
        let mut data: Vec<u8> = vec![b'B', b'M'];
        data.extend_from_slice(&[
            0xCF, 0xF0, 0xE0, 0xE9, 0xF1, 0x20, 0xEA, 0xEE, 0xEC, 0xEF, 0xE0, 0xED, 0xE8, 0xE8,
        ]);
        tokio::fs::write(dir.join("note.txt"), &data).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "note.txt"})).await.unwrap();

        assert!(
            result.success,
            "non-UTF-8 text starting with BM must not be rejected as an image, error: {:?}",
            result.error
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Non-UTF-8 *text* (a legacy single-byte encoding) must NOT be classified
    /// as binary: no NUL, no control glut, no image magic. It still reads
    /// leniently (lossy) until proper charset decoding lands as a follow-up.
    #[tokio::test]
    async fn file_read_reads_non_utf8_text_lossy() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_legacy_text");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // "Privet" (Cyrillic) in Windows-1251: all high bytes, but no NUL / control / magic.
        let cp1251: Vec<u8> = vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
        tokio::fs::write(dir.join("note.txt"), &cp1251)
            .await
            .unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "note.txt"})).await.unwrap();

        assert!(
            result.success,
            "non-UTF-8 text must not be rejected as binary, error: {:?}",
            result.error
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    // ── E2E: full agent pipeline with real FileReadTool + PDF extraction ──

    mod e2e_helpers {
        use crate::observability::{NoopObserver, Observer};
        use std::sync::{Arc, Mutex};
        use zeroclaw_config::schema::MemoryConfig;
        use zeroclaw_memory::{self, Memory};
        use zeroclaw_providers::{ChatMessage, ChatRequest, ChatResponse, ModelProvider};

        pub type SharedRequests = Arc<Mutex<Vec<Vec<ChatMessage>>>>;

        pub struct RecordingModelProvider {
            responses: Mutex<Vec<ChatResponse>>,
            pub requests: SharedRequests,
        }

        impl RecordingModelProvider {
            pub fn new(responses: Vec<ChatResponse>) -> (Self, SharedRequests) {
                let requests: SharedRequests = Arc::new(Mutex::new(Vec::new()));
                let model_provider = Self {
                    responses: Mutex::new(responses),
                    requests: requests.clone(),
                };
                (model_provider, requests)
            }
        }

        #[async_trait::async_trait]
        impl ModelProvider for RecordingModelProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                Ok("fallback".into())
            }

            async fn chat(
                &self,
                request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<ChatResponse> {
                self.requests
                    .lock()
                    .unwrap()
                    .push(request.messages.to_vec());

                let mut guard = self.responses.lock().unwrap();
                if guard.is_empty() {
                    return Ok(ChatResponse {
                        text: Some("done".into()),
                        tool_calls: vec![],
                        usage: None,
                        reasoning_content: None,
                    });
                }
                Ok(guard.remove(0))
            }
        }
        impl ::zeroclaw_api::attribution::Attributable for RecordingModelProvider {
            fn role(&self) -> ::zeroclaw_api::attribution::Role {
                ::zeroclaw_api::attribution::Role::Provider(
                    ::zeroclaw_api::attribution::ProviderKind::Model(
                        ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                    ),
                )
            }
            fn alias(&self) -> &str {
                "RecordingModelProvider"
            }
        }

        pub fn make_memory() -> Arc<dyn Memory> {
            let cfg = MemoryConfig {
                backend: "none".into(),
                ..MemoryConfig::default()
            };
            Arc::from(zeroclaw_memory::create_memory(&cfg, &std::env::temp_dir(), None).unwrap())
        }

        pub fn make_observer() -> Arc<dyn Observer> {
            Arc::from(NoopObserver {})
        }
    }

    /// End-to-end test: scripted model_provider calls `file_read` on a real PDF
    /// fixture, the tool extracts text via pdf-extract, and the extracted
    /// content reaches the model_provider in the tool result message.
    #[tokio::test]
    async fn e2e_agent_file_read_pdf_extraction() {
        use crate::agent::agent::Agent;
        use crate::agent::dispatcher::NativeToolDispatcher;
        use e2e_helpers::*;
        use zeroclaw_providers::{ChatResponse, ModelProvider, ToolCall};

        // ── Set up workspace with PDF fixture ──
        let workspace = std::env::temp_dir().join("zeroclaw_test_e2e_file_read_pdf");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/test_document.pdf");
        tokio::fs::copy(&fixture, workspace.join("report.pdf"))
            .await
            .expect("copy PDF fixture");

        // ── Build real FileReadTool ──
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let file_read_tool: Box<dyn Tool> = Box::new(FileReadTool::new(security));

        // ── Script model_provider: call file_read → then answer ──
        let (model_provider, recorded) = RecordingModelProvider::new(vec![
            // Turn 1 response: model_provider asks to read the PDF
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "file_read".into(),
                    arguments: r#"{"path": "report.pdf"}"#.into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            },
            // Turn 1 continued: model_provider sees tool result and answers
            ChatResponse {
                text: Some("The PDF contains a greeting: Hello PDF".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);

        let mut agent = Agent::builder()
            .model_provider(Box::new(model_provider) as Box<dyn ModelProvider>)
            .tools(vec![file_read_tool])
            .memory(make_memory())
            .observer(make_observer())
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(workspace.clone())
            .build()
            .unwrap();

        // ── Execute ──
        let response = agent
            .turn("Read report.pdf and tell me what it says")
            .await
            .unwrap();

        // ── Verify final response ──
        assert!(
            response.contains("Hello PDF"),
            "agent response must contain PDF content, got: {response}",
        );

        // ── Verify model_provider received extracted PDF text in tool result ──
        {
            let all_requests = recorded.lock().unwrap();
            assert!(
                all_requests.len() >= 2,
                "expected at least 2 model_provider requests (initial + after tool), got {}",
                all_requests.len(),
            );

            let second_request = &all_requests[1];
            let tool_result_msg = second_request
                .iter()
                .find(|m| m.role == "tool")
                .expect("second request must contain a tool result message");

            assert!(
                tool_result_msg.content.contains("Hello"),
                "tool result must contain extracted PDF text 'Hello', got: {}",
                tool_result_msg.content,
            );
        }

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }

    /// End-to-end test: agent calls `file_read` on a binary file and gets a
    /// binary-rejection error in the tool result (no lossy replacement output).
    #[tokio::test]
    async fn e2e_agent_file_read_rejects_binary() {
        use crate::agent::agent::Agent;
        use crate::agent::dispatcher::NativeToolDispatcher;
        use e2e_helpers::*;
        use zeroclaw_providers::{ChatResponse, ModelProvider, ToolCall};

        // ── Set up workspace with binary file ──
        let workspace = std::env::temp_dir().join("zeroclaw_test_e2e_file_read_lossy");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let binary_data: Vec<u8> = vec![0x00, 0x80, 0xFF, 0xFE, b'v', b'a', b'l', b'i', b'd', 0x80];
        tokio::fs::write(workspace.join("data.bin"), &binary_data)
            .await
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let file_read_tool: Box<dyn Tool> = Box::new(FileReadTool::new(security));

        let (model_provider, recorded) = RecordingModelProvider::new(vec![
            ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "file_read".into(),
                    arguments: r#"{"path": "data.bin"}"#.into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            },
            ChatResponse {
                text: Some("The file appears to be binary data.".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]);

        let mut agent = Agent::builder()
            .model_provider(Box::new(model_provider) as Box<dyn ModelProvider>)
            .tools(vec![file_read_tool])
            .memory(make_memory())
            .observer(make_observer())
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(workspace.clone())
            .build()
            .unwrap();

        let response = agent.turn("Read data.bin").await.unwrap();

        assert!(
            response.contains("binary"),
            "agent response must mention binary, got: {response}",
        );

        // Verify the tool result carries the binary-rejection error, not lossy output
        {
            let all_requests = recorded.lock().unwrap();
            assert!(
                all_requests.len() >= 2,
                "expected at least 2 model_provider requests, got {}",
                all_requests.len(),
            );

            let tool_result_msg = all_requests[1]
                .iter()
                .find(|m| m.role == "tool")
                .expect("second request must contain a tool result message");

            assert!(
                tool_result_msg.content.contains("Binary file detected"),
                "tool result must contain the binary-rejection error, got: {}",
                tool_result_msg.content,
            );
            assert!(
                !tool_result_msg.content.contains('\u{FFFD}'),
                "tool result must NOT contain lossy replacement characters, got: {}",
                tool_result_msg.content,
            );
        }

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }

    /// Live e2e: real OpenAI Codex model_provider + real FileReadTool + PDF fixture.
    /// Verifies the model receives extracted PDF text and responds meaningfully.
    ///
    /// Requires valid OAuth credentials in `~/.zeroclaw/`.
    /// Run: `cargo test --lib -- tools::file_read::tests::e2e_live_file_read_pdf --ignored --nocapture`
    #[tokio::test]
    #[ignore = "requires valid OpenAI Codex OAuth credentials"]
    async fn e2e_live_file_read_pdf() {
        use crate::agent::agent::Agent;
        use crate::agent::dispatcher::XmlToolDispatcher;
        use e2e_helpers::*;
        use zeroclaw_providers::openai_codex::OpenAiCodexModelProvider;
        use zeroclaw_providers::{ModelProvider, ModelProviderRuntimeOptions};

        // ── Set up workspace with PDF fixture ──
        let workspace = std::env::temp_dir().join("zeroclaw_test_e2e_live_file_read_pdf");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();

        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/test_document.pdf");
        tokio::fs::copy(&fixture, workspace.join("report.pdf"))
            .await
            .expect("copy PDF fixture");

        // ── Build real FileReadTool ──
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let file_read_tool: Box<dyn Tool> = Box::new(FileReadTool::new(security));

        // ── Real model_provider (OpenAI Codex uses XML tool dispatch) ──
        let model_provider =
            OpenAiCodexModelProvider::new("test", &ModelProviderRuntimeOptions::default(), None)
                .expect("model_provider should initialize");

        let mut agent = Agent::builder()
            .model_provider(Box::new(model_provider) as Box<dyn ModelProvider>)
            .tools(vec![file_read_tool])
            .memory(make_memory())
            .observer(make_observer())
            .tool_dispatcher(Box::new(XmlToolDispatcher))
            .workspace_dir(workspace.clone())
            .model_name("gpt-5.3-codex".to_string())
            .build()
            .unwrap();

        // ── Execute ──
        let response = agent
            .turn("Use the file_read tool to read report.pdf, then tell me what text it contains. Be concise.")
            .await
            .unwrap();

        eprintln!("=== Live e2e response ===\n{response}\n=========================");

        // ── Verify model saw the actual PDF content ("Hello PDF") ──
        let lower = response.to_lowercase();
        assert!(
            lower.contains("hello"),
            "model response must reference extracted PDF text 'Hello PDF', got: {response}",
        );

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }

    #[tokio::test]
    async fn file_read_blocks_null_byte_in_path() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_null_byte");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool
            .execute(json!({"path": "test\0evil.txt"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_read_allows_dev_null() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_dev_null");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let tool = test_tool(dir.clone());
        let result = tool.execute(json!({"path": "/dev/null"})).await.unwrap();

        assert!(
            result.success,
            "file_read of /dev/null must succeed, error: {:?}",
            result.error
        );
        assert_eq!(result.output, "", "/dev/null must read as empty");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_allowed_root_with_workspace_only() {
        let root = std::env::temp_dir().join("zeroclaw_test_file_read_allowed_root");
        let workspace = root.join("workspace");
        let allowed = root.join("allowed_dir");

        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&allowed).await.unwrap();
        tokio::fs::write(allowed.join("data.txt"), "allowed content")
            .await
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace.clone(),
            workspace_only: true,
            allowed_roots: vec![allowed.clone()],
            ..SecurityPolicy::default()
        });
        let tool = FileReadTool::new(security);

        // Absolute path under allowed_root should succeed
        let abs_path = allowed.join("data.txt").to_string_lossy().to_string();
        let result = tool.execute(json!({"path": &abs_path})).await.unwrap();

        assert!(
            result.success,
            "file_read with allowed_root path should succeed, error: {:?}",
            result.error
        );
        assert!(result.output.contains("allowed content"));

        // Path outside both workspace and allowed_roots should still fail
        let outside = root.join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("secret.txt"), "secret")
            .await
            .unwrap();
        let outside_path = outside.join("secret.txt").to_string_lossy().to_string();
        let result = tool.execute(json!({"path": &outside_path})).await.unwrap();
        assert!(!result.success);

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    /// Anti-probing regression: a caller cannot probe file existence for free.
    /// Both `resolve_candidate` failures and `canonicalize` failures must
    /// consume one action-budget slot, so repeated probes hit the rate limit.
    #[tokio::test]
    async fn file_read_nonexistent_consumes_rate_limit_budget() {
        let dir = std::env::temp_dir().join("zeroclaw_test_file_read_probe");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // Allow only 2 actions total.
        let tool = test_tool_with(dir.clone(), AutonomyLevel::Supervised, 2);

        // Two failing reads each consume one slot via the inner-tool charge.
        let r1 = tool.execute(json!({"path": "nope1.txt"})).await.unwrap();
        assert!(!r1.success);
        assert!(
            r1.error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to resolve")
        );

        let r2 = tool.execute(json!({"path": "nope2.txt"})).await.unwrap();
        assert!(!r2.success);
        assert!(
            r2.error
                .as_deref()
                .unwrap_or("")
                .contains("Failed to resolve")
        );

        // Third attempt: budget is now exhausted.  The inner tool still
        // charges, but `record_action()` returns false; the failure error
        // is unchanged from the caller's perspective (probing failed),
        // and the budget is observably full (a subsequent allowed read
        // would have to wait for the window to reset).
        let r3 = tool.execute(json!({"path": "nope3.txt"})).await.unwrap();
        assert!(!r3.success);

        // Verify the budget is actually full by attempting a real read,
        // which must now report rate-limit exhaustion when wrapped, or at
        // minimum fail.  Here we use the inner-only tool, so we just
        // assert that record_action returns false (budget already at cap).
        // The inner tool's own retry would consume nothing more.
        assert!(!tool.security.record_action(), "budget must be exhausted");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
