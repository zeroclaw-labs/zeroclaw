use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Upper bound on the image file size we will read for metadata extraction.
///
/// This is a coarse safety ceiling, not the multimodal size policy. The
/// per-request decision on whether an image is small enough to inline for a
/// vision model is the pipeline's `multimodal.max_image_size_mb`
/// (`MultimodalConfig::effective_limits`, clamped to 1..=20 MB). We size this
/// ceiling to that clamp's upper bound (20 MiB) so `image_info` never refuses
/// to read — and therefore never silently withholds metadata for — a file the
/// pipeline would otherwise have been configured to accept. When the pipeline
/// limit is lower, the pipeline does the rejecting (with a model-facing note);
/// `image_info` still returns the metadata text either way.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Tool to read image metadata and expose the image to vision-capable models.
///
/// Extracts file size, format, and dimensions from header bytes, and emits an
/// `[IMAGE:<absolute path>]` marker so the multimodal pipeline inlines the
/// image bytes for the next provider call when the model supports vision.
pub struct ImageInfoTool {
    // Pre-canonicalization path-allowlist enforcement lives in the
    // PathGuardedTool wrapper. The concrete tool still resolves raw tool
    // paths and applies the read-side post-canonicalization boundary.
    security: Arc<SecurityPolicy>,
}

impl ImageInfoTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    /// Strip the Windows verbatim (`\\?\`) prefix that `canonicalize` prepends
    /// on Windows, so the emitted `[IMAGE:]` marker carries a plain
    /// drive-letter path (`C:\…`) instead of `\\?\C:\…`.
    ///
    /// This matters because the multimodal pipeline's path detector
    /// (`zeroclaw-providers::multimodal::is_windows_path`) only recognizes
    /// paths beginning with a drive letter; the leading backslashes of the
    /// verbatim form make it reject the marker, so the image would never be
    /// inlined for vision-capable models. The verbatim UNC form
    /// (`\\?\UNC\server\share\…`) is unwrapped back to its `\\server\share\…`
    /// spelling. Inputs without a verbatim prefix (e.g. all POSIX paths) are
    /// returned unchanged and without allocating.
    fn strip_windows_verbatim_prefix(path: &str) -> std::borrow::Cow<'_, str> {
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            std::borrow::Cow::Owned(format!(r"\\{rest}"))
        } else if let Some(rest) = path.strip_prefix(r"\\?\") {
            std::borrow::Cow::Borrowed(rest)
        } else {
            std::borrow::Cow::Borrowed(path)
        }
    }

    /// Detect image format from first few bytes (magic numbers).
    fn detect_format(bytes: &[u8]) -> &'static str {
        if bytes.len() < 4 {
            return "unknown";
        }
        if bytes.starts_with(b"\x89PNG") {
            "png"
        } else if bytes.starts_with(b"\xFF\xD8\xFF") {
            "jpeg"
        } else if bytes.starts_with(b"GIF8") {
            "gif"
        } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
            "webp"
        } else if bytes.starts_with(b"BM") {
            "bmp"
        } else {
            "unknown"
        }
    }

    /// Try to extract dimensions from image header bytes.
    /// Returns (width, height) if detectable.
    fn extract_dimensions(bytes: &[u8], format: &str) -> Option<(u32, u32)> {
        match format {
            "png" => {
                // PNG IHDR chunk: bytes 16-19 = width, 20-23 = height (big-endian)
                if bytes.len() >= 24 {
                    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
                    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
                    Some((w, h))
                } else {
                    None
                }
            }
            "gif" => {
                // GIF: bytes 6-7 = width, 8-9 = height (little-endian)
                if bytes.len() >= 10 {
                    let w = u32::from(u16::from_le_bytes([bytes[6], bytes[7]]));
                    let h = u32::from(u16::from_le_bytes([bytes[8], bytes[9]]));
                    Some((w, h))
                } else {
                    None
                }
            }
            "bmp" => {
                // BMP: bytes 18-21 = width, 22-25 = height (little-endian, signed)
                if bytes.len() >= 26 {
                    let w = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
                    let h_raw = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
                    let h = h_raw.unsigned_abs();
                    Some((w, h))
                } else {
                    None
                }
            }
            "jpeg" => Self::jpeg_dimensions(bytes),
            _ => None,
        }
    }

    /// Parse JPEG SOF markers to extract dimensions.
    fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
        let mut i = 2; // skip SOI marker
        while i + 1 < bytes.len() {
            if bytes[i] != 0xFF {
                return None;
            }
            let marker = bytes[i + 1];
            i += 2;

            // SOF0..SOF3 markers contain dimensions
            if (0xC0..=0xC3).contains(&marker) {
                if i + 7 <= bytes.len() {
                    let h = u32::from(u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]));
                    let w = u32::from(u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]));
                    return Some((w, h));
                }
                return None;
            }

            // Skip this segment
            if i + 1 < bytes.len() {
                let seg_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
                if seg_len < 2 {
                    return None; // Malformed segment (valid segments have length >= 2)
                }
                i += seg_len;
            } else {
                return None;
            }
        }
        None
    }
}

#[async_trait]
impl Tool for ImageInfoTool {
    fn name(&self) -> &str {
        "image_info"
    }

    fn description(&self) -> &str {
        "Read image file metadata (format, dimensions, size). The image is also made available to vision-capable models via an inline image marker."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file (absolute or relative to workspace)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "path"})),
                "image_info: missing path parameter"
            );
            anyhow::Error::msg("Missing 'path' parameter")
        })?;

        // Path-allowlist checks are applied by the PathGuardedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod). Successful
        // reads consume budget through RateLimitedTool; post-wrapper
        // canonicalize failures are charged here so missing-file probes are not
        // free.

        let full_path = self.security.resolve_tool_path(path_str);
        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(path) => path,
            Err(e) => {
                let _ = self.security.record_action();
                let error = if e.kind() == std::io::ErrorKind::NotFound {
                    format!("File not found: {path_str}")
                } else {
                    format!("Failed to resolve file path: {e}")
                };
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error),
                });
            }
        };

        if !self.security.is_resolved_path_readable(&resolved_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Resolved image path is outside the allowed readable roots.".to_string(),
                ),
            });
        }

        let metadata = tokio::fs::metadata(&resolved_path).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path_str,
                        "error": format!("{}", e),
                    })),
                "image_info: failed to read file metadata"
            );
            anyhow::Error::msg(format!("Failed to read file metadata: {e}"))
        })?;

        let file_size = metadata.len();

        if file_size > MAX_IMAGE_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Image too large: {file_size} bytes (max {MAX_IMAGE_BYTES} bytes)"
                )),
            });
        }

        let bytes = tokio::fs::read(&resolved_path).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path_str,
                        "error": format!("{}", e),
                    })),
                "image_info: failed to read image file"
            );
            anyhow::Error::msg(format!("Failed to read image file: {e}"))
        })?;

        let format = Self::detect_format(&bytes);
        let dimensions = Self::extract_dimensions(&bytes, format);

        // We emit two things for the resolved image:
        //   1. A durable `File: <absolute path>` line, and
        //   2. A standalone `[IMAGE:<absolute path>]` marker
        // both using the canonicalized absolute path (not the caller-supplied
        // `path_str`, which may be workspace-relative — the tool-result marker
        // promoter only recognizes absolute paths, so a relative path would be
        // silently dropped and never reach the model; see issue #7436).
        //
        // The `[IMAGE:]` marker is what the multimodal pipeline inlines for
        // vision models, but it is stripped from older turns to control
        // context size. The separate `File:` line keeps the path visible in
        // history *after* the marker is gone, so the model retains the path
        // (and can re-read the file via `image_info`) across turns. Emitting
        // the same path twice is safe: the promoter
        // (`canonicalize_tool_result_media_markers`) dedups a bare path that
        // already appears inside an explicit marker, so the `File:` line is
        // not wrapped into a second, double-counted marker.
        //
        // On Windows `canonicalize` returns a verbatim path (`\\?\C:\…`); we
        // strip that prefix so both the `File:` line and the marker carry a
        // plain `C:\…` path the multimodal pipeline's `is_windows_path`
        // detector accepts. Using the identical string for both also keeps the
        // promoter's dedup exact. See #7436 (Windows follow-up to #7446).
        let resolved_display = resolved_path.display().to_string();
        let marker_path = Self::strip_windows_verbatim_prefix(&resolved_display);
        let mut output = format!("File: {marker_path}\nFormat: {format}\nSize: {file_size} bytes");

        if let Some((w, h)) = dimensions {
            let _ = write!(output, "\nDimensions: {w}x{h}");
        }

        let _ = write!(output, "\n[IMAGE:{marker_path}]");

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
    use crate::wrappers::{PathGuardedTool, RateLimitedTool};
    use std::path::{Component, Path, PathBuf};
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE2, 0x21, 0xBC, 0x33, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            workspace_only: false,
            forbidden_paths: vec![],
            ..SecurityPolicy::default()
        })
    }

    /// Security policy with `workspace_only: true` so external absolute paths
    /// are blocked by the `PathGuardedTool` wrapper.
    fn workspace_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace,
            workspace_only: true,
            ..SecurityPolicy::default()
        })
    }

    fn rootless_path(path: &Path) -> PathBuf {
        let mut relative = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
                Component::ParentDir => panic!("test path must not contain parent components"),
                Component::Normal(part) => relative.push(part),
            }
        }
        relative
    }

    /// Wraps `ImageInfoTool` with the production `PathGuardedTool` +
    /// `RateLimitedTool` stack, mirroring the registration in
    /// `zeroclaw-runtime::tools::mod`.  Use this in tests that exercise
    /// path-allowlist or rate-limit behavior.
    fn wrapped_tool(workspace: std::path::PathBuf) -> Box<dyn Tool> {
        let security = workspace_security(workspace);
        wrapped_tool_with_security(security)
    }

    fn wrapped_tool_with_security(security: Arc<SecurityPolicy>) -> Box<dyn Tool> {
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(ImageInfoTool::new(security.clone()), security.clone()),
            security,
        ))
    }

    #[test]
    fn image_info_tool_name() {
        let tool = ImageInfoTool::new(test_security());
        assert_eq!(tool.name(), "image_info");
    }

    #[test]
    fn image_info_tool_description() {
        let tool = ImageInfoTool::new(test_security());
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("image"));
    }

    #[test]
    fn image_info_tool_schema() {
        let tool = ImageInfoTool::new(test_security());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        // `include_base64` was removed: the image now reaches vision models via
        // an inline `[IMAGE:]` marker, not a bare base64 blob (issue #7436).
        assert!(schema["properties"]["include_base64"].is_null());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[test]
    fn image_info_tool_spec() {
        let tool = ImageInfoTool::new(test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "image_info");
        assert!(spec.parameters.is_object());
    }

    // ── Windows verbatim-prefix stripping ───────────────────────

    #[test]
    fn strip_verbatim_disk_prefix() {
        // `canonicalize` on Windows yields `\\?\C:\…`; the marker must carry
        // the plain drive-letter path so `is_windows_path` accepts it.
        assert_eq!(
            ImageInfoTool::strip_windows_verbatim_prefix(r"\\?\C:\Users\me\Downloads\a.png"),
            r"C:\Users\me\Downloads\a.png"
        );
    }

    #[test]
    fn strip_verbatim_unc_prefix() {
        // Verbatim UNC unwraps back to the `\\server\share\…` spelling.
        assert_eq!(
            ImageInfoTool::strip_windows_verbatim_prefix(r"\\?\UNC\server\share\pic.png"),
            r"\\server\share\pic.png"
        );
    }

    #[test]
    fn strip_verbatim_prefix_leaves_plain_paths_unchanged() {
        // POSIX paths and already-plain Windows paths must pass through
        // untouched (and without allocating).
        for input in [
            "/home/me/pictures/a.png",
            r"C:\Users\me\a.png",
            "relative/a.png",
        ] {
            assert!(matches!(
                ImageInfoTool::strip_windows_verbatim_prefix(input),
                std::borrow::Cow::Borrowed(_)
            ));
            assert_eq!(ImageInfoTool::strip_windows_verbatim_prefix(input), input);
        }
    }

    // ── Format detection ────────────────────────────────────────

    #[test]
    fn detect_png() {
        let bytes = b"\x89PNG\r\n\x1a\n";
        assert_eq!(ImageInfoTool::detect_format(bytes), "png");
    }

    #[test]
    fn detect_jpeg() {
        let bytes = b"\xFF\xD8\xFF\xE0";
        assert_eq!(ImageInfoTool::detect_format(bytes), "jpeg");
    }

    #[test]
    fn detect_gif() {
        let bytes = b"GIF89a";
        assert_eq!(ImageInfoTool::detect_format(bytes), "gif");
    }

    #[test]
    fn detect_webp() {
        let bytes = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(ImageInfoTool::detect_format(bytes), "webp");
    }

    #[test]
    fn detect_bmp() {
        let bytes = b"BM\x00\x00";
        assert_eq!(ImageInfoTool::detect_format(bytes), "bmp");
    }

    #[test]
    fn detect_unknown_short() {
        let bytes = b"\x00\x01";
        assert_eq!(ImageInfoTool::detect_format(bytes), "unknown");
    }

    #[test]
    fn detect_unknown_garbage() {
        let bytes = b"this is not an image";
        assert_eq!(ImageInfoTool::detect_format(bytes), "unknown");
    }

    // ── Dimension extraction ────────────────────────────────────

    #[test]
    fn png_dimensions() {
        // Minimal PNG IHDR: 8-byte signature + 4-byte length + 4-byte IHDR + 4-byte width + 4-byte height
        let mut bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // IHDR length
            0x49, 0x48, 0x44, 0x52, // "IHDR"
            0x00, 0x00, 0x03, 0x20, // width: 800
            0x00, 0x00, 0x02, 0x58, // height: 600
        ];
        bytes.extend_from_slice(&[0u8; 10]); // padding
        let dims = ImageInfoTool::extract_dimensions(&bytes, "png");
        assert_eq!(dims, Some((800, 600)));
    }

    #[test]
    fn gif_dimensions() {
        let bytes = [
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // GIF89a
            0x40, 0x01, // width: 320 (LE)
            0xF0, 0x00, // height: 240 (LE)
        ];
        let dims = ImageInfoTool::extract_dimensions(&bytes, "gif");
        assert_eq!(dims, Some((320, 240)));
    }

    #[test]
    fn bmp_dimensions() {
        let mut bytes = vec![0u8; 26];
        bytes[0] = b'B';
        bytes[1] = b'M';
        // width at offset 18 (LE): 1024
        bytes[18] = 0x00;
        bytes[19] = 0x04;
        bytes[20] = 0x00;
        bytes[21] = 0x00;
        // height at offset 22 (LE): 768
        bytes[22] = 0x00;
        bytes[23] = 0x03;
        bytes[24] = 0x00;
        bytes[25] = 0x00;
        let dims = ImageInfoTool::extract_dimensions(&bytes, "bmp");
        assert_eq!(dims, Some((1024, 768)));
    }

    #[test]
    fn jpeg_dimensions() {
        // Minimal JPEG-like byte sequence with SOF0 marker
        let mut bytes: Vec<u8> = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, // APP0 marker
            0x00, 0x10, // APP0 length = 16
        ];
        bytes.extend_from_slice(&[0u8; 14]); // APP0 payload
        bytes.extend_from_slice(&[
            0xFF, 0xC0, // SOF0 marker
            0x00, 0x11, // SOF0 length
            0x08, // precision
            0x01, 0xE0, // height: 480
            0x02, 0x80, // width: 640
        ]);
        let dims = ImageInfoTool::extract_dimensions(&bytes, "jpeg");
        assert_eq!(dims, Some((640, 480)));
    }

    #[test]
    fn jpeg_malformed_zero_length_segment() {
        // Zero-length segment should return None instead of looping forever
        let bytes: Vec<u8> = vec![
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, // APP0 marker
            0x00, 0x00, // length = 0 (malformed)
        ];
        let dims = ImageInfoTool::extract_dimensions(&bytes, "jpeg");
        assert!(dims.is_none());
    }

    #[test]
    fn unknown_format_no_dimensions() {
        let bytes = b"random data here";
        let dims = ImageInfoTool::extract_dimensions(bytes, "unknown");
        assert!(dims.is_none());
    }

    // ── Execute tests ───────────────────────────────────────────

    #[tokio::test]
    async fn execute_missing_path() {
        let tool = ImageInfoTool::new(test_security());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_nonexistent_file() {
        let tool = ImageInfoTool::new(test_security());
        let result = tool
            .execute(json!({"path": "/tmp/nonexistent_image_xyz.png"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn execute_real_file() {
        let dir = TempDir::new().unwrap();
        let png_path = dir.path().join("test.png");
        tokio::fs::write(&png_path, MINIMAL_PNG).await.unwrap();

        let tool = ImageInfoTool::new(test_security());
        let result = tool
            .execute(json!({"path": png_path.to_string_lossy()}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Format: png"));
        assert!(result.output.contains("Dimensions: 1x1"));
        assert!(!result.output.contains("data:"));
        // The output carries an absolute-path [IMAGE:] marker so the
        // multimodal pipeline can inline the image for vision models.
        let canonical = tokio::fs::canonicalize(&png_path).await.unwrap();
        assert!(
            result
                .output
                .contains(&format!("[IMAGE:{}]", canonical.display())),
            "expected absolute-path image marker, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn wrapped_blocks_external_absolute_path() {
        // Regression for the removed inline path check: when ImageInfoTool is
        // composed with PathGuardedTool (as it is in production), an external
        // absolute path must be blocked before the inner tool runs.
        let workspace = std::env::temp_dir().join("zeroclaw_image_info_wrap");
        let _ = std::fs::create_dir_all(&workspace);
        let tool = wrapped_tool(workspace);

        #[cfg(unix)]
        let target = "/etc/passwd";
        #[cfg(windows)]
        let target = {
            let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            format!(r"{sysroot}\System32\drivers\etc\hosts")
        };

        let result = tool.execute(json!({"path": target})).await.unwrap();
        assert!(!result.success, "external path must be blocked");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked"),
            "expected 'Path blocked' error, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn wrapped_blocks_path_traversal() {
        // Path-traversal under workspace_only must be blocked by the wrapper,
        // not pass through to the inner tool.
        let workspace = std::env::temp_dir().join("zeroclaw_image_info_trav");
        let _ = std::fs::create_dir_all(&workspace);
        let tool = wrapped_tool(workspace);

        let result = tool
            .execute(json!({"path": "../../../etc/passwd"}))
            .await
            .unwrap();
        assert!(!result.success, "path traversal must be blocked");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked"),
            "expected 'Path blocked' error, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn wrapped_normalizes_workspace_prefixed_relative_path() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("zeroclaw-data").join("workspace");
        let images_dir = workspace.join("images");
        tokio::fs::create_dir_all(&images_dir).await.unwrap();

        let png_path = images_dir.join("one.png");
        tokio::fs::write(&png_path, MINIMAL_PNG).await.unwrap();

        let workspace_prefixed = rootless_path(&workspace).join("images").join("one.png");
        let tool = wrapped_tool(workspace);

        let result = tool
            .execute(json!({"path": workspace_prefixed.to_string_lossy()}))
            .await
            .unwrap();

        assert!(
            result.success,
            "workspace-prefixed image path should resolve through security policy, error: {:?}",
            result.error
        );
        assert!(result.output.contains("Format: png"));
        // Regression for issue #7436: a workspace-relative path must still be
        // emitted as an absolute-path [IMAGE:] marker. Before the fix the tool
        // echoed the relative input, which the marker promoter (anchored on a
        // leading `/`) silently dropped, so the image never reached the model.
        let canonical = tokio::fs::canonicalize(&png_path).await.unwrap();
        assert!(
            result
                .output
                .contains(&format!("[IMAGE:{}]", canonical.display())),
            "expected absolute-path image marker, got: {}",
            result.output
        );
        assert!(
            canonical.is_absolute(),
            "marker path must be absolute so the multimodal pipeline can load it"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn wrapped_blocks_symlink_escape_after_resolution() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("secret.png"), MINIMAL_PNG)
            .await
            .unwrap();
        symlink(outside.join("secret.png"), workspace.join("link.png")).unwrap();

        let tool = wrapped_tool(workspace);
        let result = tool.execute(json!({"path": "link.png"})).await.unwrap();

        assert!(!result.success, "symlink escape must be blocked");
        let error = result.error.as_deref().unwrap_or("");
        assert!(
            error.contains("outside the allowed readable roots"),
            "expected readable-roots error, got: {:?}",
            error
        );
        assert!(
            !error.contains(&outside.to_string_lossy().to_string()),
            "policy error must not disclose resolved outside path, got: {error}"
        );
    }

    #[tokio::test]
    async fn wrapped_blocks_write_only_allowed_root_read() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let write_only = root.path().join("write-only");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&write_only).await.unwrap();
        let png_path = write_only.join("one.png");
        tokio::fs::write(&png_path, MINIMAL_PNG).await.unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace,
            workspace_only: true,
            allowed_roots_write_only: vec![write_only],
            ..SecurityPolicy::default()
        });
        let tool = wrapped_tool_with_security(security);
        let result = tool
            .execute(json!({"path": png_path.to_string_lossy()}))
            .await
            .unwrap();

        assert!(!result.success, "write-only root must not be readable");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("outside the allowed readable roots"),
            "expected readable-roots error, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn missing_file_probe_consumes_action_budget() {
        let root = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: root.path().to_path_buf(),
            workspace_only: true,
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        });
        let tool = ImageInfoTool::new(security.clone());

        assert!(!security.is_rate_limited());
        let result = tool.execute(json!({"path": "missing.png"})).await.unwrap();

        assert!(!result.success);
        assert!(security.is_rate_limited());
    }

    #[tokio::test]
    async fn emits_inline_image_marker_with_absolute_path() {
        // The image must be exposed to vision models via an [IMAGE:] marker
        // carrying the canonical absolute path, regardless of how the caller
        // spelled the input path (issue #7436).
        let dir = TempDir::new().unwrap();
        let png_path = dir.path().join("marker.png");
        tokio::fs::write(&png_path, MINIMAL_PNG).await.unwrap();

        let tool = ImageInfoTool::new(test_security());
        let result = tool
            .execute(json!({"path": png_path.to_string_lossy()}))
            .await
            .unwrap();

        assert!(result.success);
        let canonical = tokio::fs::canonicalize(&png_path).await.unwrap();
        assert!(
            result
                .output
                .contains(&format!("[IMAGE:{}]", canonical.display())),
            "expected absolute-path image marker, got: {}",
            result.output
        );
        // No bare base64 blob should leak into the text output anymore.
        assert!(!result.output.contains("base64,"));
    }
}
