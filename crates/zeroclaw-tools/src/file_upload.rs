use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::FileUploadConfig;

const RESPONSE_BODY_LIMIT_BYTES: usize = 4 * 1024;

pub struct FileUploadTool {
    security: Arc<SecurityPolicy>,
    config: FileUploadConfig,
}

impl FileUploadTool {
    pub fn new(security: Arc<SecurityPolicy>, config: FileUploadConfig) -> Self {
        Self { security, config }
    }

    /// Best-effort MIME detection. Tries content-sniffing on the first bytes
    /// (catches binary files with wrong or missing extensions), then falls
    /// back to a filename-extension table for text formats and types `infer`
    /// does not cover, then finally to `application/octet-stream`.
    fn detect_mime(bytes: &[u8], file_name: &str) -> &'static str {
        if let Some(kind) = infer::get(bytes) {
            return kind.mime_type();
        }
        Self::mime_for_filename(file_name)
    }

    fn mime_for_filename(name: &str) -> &'static str {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            // Images
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "tiff" | "tif" => "image/tiff",
            "svg" => "image/svg+xml",
            "heic" => "image/heic",
            "avif" => "image/avif",
            "ico" => "image/x-icon",
            // Documents
            "pdf" => "application/pdf",
            "rtf" => "application/rtf",
            "doc" => "application/msword",
            "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "xls" => "application/vnd.ms-excel",
            "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "ppt" => "application/vnd.ms-powerpoint",
            "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "odt" => "application/vnd.oasis.opendocument.text",
            "ods" => "application/vnd.oasis.opendocument.spreadsheet",
            "epub" => "application/epub+zip",
            // Data / structured
            "json" => "application/json",
            "xml" => "application/xml",
            "yaml" | "yml" => "application/yaml",
            "toml" => "application/toml",
            "sql" => "application/sql",
            // Archives
            "zip" => "application/zip",
            "tar" => "application/x-tar",
            "gz" | "tgz" => "application/gzip",
            "bz2" => "application/x-bzip2",
            "xz" => "application/x-xz",
            "7z" => "application/x-7z-compressed",
            "rar" => "application/vnd.rar",
            // Code / text
            "txt" | "log" => "text/plain",
            "md" | "markdown" => "text/markdown",
            "csv" => "text/csv",
            "tsv" => "text/tab-separated-values",
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" | "mjs" | "cjs" => "application/javascript",
            "ts" => "application/typescript",
            "rs" => "text/x-rust",
            "py" => "text/x-python",
            "sh" | "bash" => "application/x-sh",
            // Audio
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "ogg" | "oga" | "opus" => "audio/ogg",
            "flac" => "audio/flac",
            // Video
            "m4a" | "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "mkv" => "video/x-matroska",
            "avi" => "video/x-msvideo",
            // Fonts
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            _ => "application/octet-stream",
        }
    }

    /// Truncate a remote response body to at most `RESPONSE_BODY_LIMIT_BYTES`,
    /// snapping the cut *down* to the nearest UTF-8 character boundary. The body
    /// is controlled by the operator-configured receiver and is untrusted input,
    /// so a multi-byte character straddling the byte limit must not panic the
    /// slice (`&body[..n]` requires `n` to be a char boundary).
    fn truncate_response_body(body: &str) -> String {
        if body.len() <= RESPONSE_BODY_LIMIT_BYTES {
            return body.to_string();
        }
        // A UTF-8 code point is at most 4 bytes, so this steps back at most 3.
        let mut end = RESPONSE_BODY_LIMIT_BYTES;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... [truncated {} bytes]", &body[..end], body.len() - end)
    }
}

#[async_trait]
impl Tool for FileUploadTool {
    fn name(&self) -> &str {
        "file_upload"
    }

    fn description(&self) -> &str {
        "Upload a local file to the configured remote endpoint via multipart/form-data. \
         The file path stays on the host; bytes are not loaded into model context. \
         Returns the HTTP status and a truncated response body so the caller can extract \
         any URL or identifier the receiver echoes back."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file on the agent's filesystem. Relative paths resolve from the workspace."
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(url) = self
            .config
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("file_upload is disabled: [file_upload].url is not configured".into()),
            });
        };

        let method = self.config.method.to_ascii_uppercase();
        if method != "POST" && method != "PUT" {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported HTTP method '{method}'. Only POST and PUT are allowed."
                )),
            });
        }

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

        let path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::Error::msg("Missing 'file_path' parameter"))?;

        if !self.security.is_path_allowed(path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path not allowed by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let full_path = self.security.resolve_tool_path(path);

        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&resolved_path),
                ),
            });
        }

        let metadata = match tokio::fs::metadata(&resolved_path).await {
            Ok(m) => m,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file metadata: {e}")),
                });
            }
        };

        if !metadata.is_file() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Not a regular file: {}", resolved_path.display())),
            });
        }

        if metadata.len() > self.config.max_file_size_bytes {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "File too large: {} bytes (limit: {} bytes)",
                    metadata.len(),
                    self.config.max_file_size_bytes
                )),
            });
        }

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

        // Re-check against the bytes actually read. The metadata guard above can
        // be defeated if the file grows between `metadata()` and `read()` (or for
        // sources whose pre-read size is unreliable), so enforce the cap on the
        // payload that would actually hit the network before building the body.
        if bytes.len() as u64 > self.config.max_file_size_bytes {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "File too large after read: {} bytes (limit: {} bytes)",
                    bytes.len(),
                    self.config.max_file_size_bytes
                )),
            });
        }

        let file_name = resolved_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("upload")
            .to_string();
        let mime = Self::detect_mime(&bytes, &file_name);

        let part = match reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.clone())
            .mime_str(mime)
        {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build multipart part: {e}")),
                });
            }
        };

        let form = reqwest::multipart::Form::new().part(self.config.field_name.clone(), part);

        let client = zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "tool.file_upload",
            self.config.timeout_secs,
            10,
        );

        let mut request = if method == "PUT" {
            client.put(url)
        } else {
            client.post(url)
        };

        for (k, v) in &self.config.headers {
            request = request.header(k.as_str(), v.as_str());
        }

        let response = match request.multipart(form).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Upload request failed: {e}")),
                });
            }
        };

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();
        let truncated = Self::truncate_response_body(&raw_body);

        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: format!("Uploaded {file_name} ({status}). Response: {truncated}"),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: truncated,
                error: Some(format!("Upload endpoint returned status {status}")),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_config::autonomy::AutonomyLevel;

    fn test_security(workspace: PathBuf, level: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour: 100,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn cfg(url: Option<String>) -> FileUploadConfig {
        FileUploadConfig {
            url,
            ..FileUploadConfig::default()
        }
    }

    #[test]
    fn tool_name_and_description() {
        let tmp = TempDir::new().unwrap();
        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload".into())),
        );
        assert_eq!(tool.name(), "file_upload");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_requires_file_path() {
        let tmp = TempDir::new().unwrap();
        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload".into())),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("file_path".into())));
    }

    #[tokio::test]
    async fn execute_fails_when_url_unset() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        fs::write(&file, b"hello").unwrap();

        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(None),
        );

        let result = tool
            .execute(json!({ "file_path": "hello.txt" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn execute_blocks_readonly_autonomy() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        fs::write(&file, b"hello").unwrap();

        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::ReadOnly),
            cfg(Some("https://example.com/upload".into())),
        );

        let result = tool
            .execute(json!({ "file_path": "hello.txt" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_rejects_file_over_size_cap() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("big.bin");
        fs::write(&file, vec![0u8; 2048]).unwrap();

        let mut config = cfg(Some("https://example.com/upload".into()));
        config.max_file_size_bytes = 1024;

        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_path": "big.bin" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("too large"));
    }

    #[tokio::test]
    async fn execute_rejects_path_outside_workspace() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let file = outside.path().join("secret.txt");
        fs::write(&file, b"nope").unwrap();

        let tool = FileUploadTool::new(
            test_security(workspace.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload".into())),
        );

        let result = tool
            .execute(json!({ "file_path": file.to_string_lossy() }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn execute_uploads_with_multipart_and_headers() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        fs::write(&file, b"hello world").unwrap();

        Mock::given(method("POST"))
            .and(path("/upload"))
            .and(header("X-Auth", "Bearer xyz"))
            .respond_with(
                ResponseTemplate::new(201).set_body_string(r#"{"id":"abc123","ok":true}"#),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut headers = HashMap::new();
        headers.insert("X-Auth".into(), "Bearer xyz".into());
        let config = FileUploadConfig {
            url: Some(format!("{}/upload", server.uri())),
            headers,
            ..FileUploadConfig::default()
        };

        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_path": "hello.txt" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        assert!(result.output.contains("hello.txt"));
        assert!(result.output.contains("abc123"));
    }

    #[tokio::test]
    async fn execute_reports_non_2xx_response() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        fs::write(&file, b"hello").unwrap();

        Mock::given(method("POST"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileUploadConfig {
            url: Some(format!("{}/upload", server.uri())),
            ..FileUploadConfig::default()
        };

        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_path": "hello.txt" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("403"), "unexpected error: {err}");
    }

    #[test]
    fn mime_table_covers_common_extensions() {
        assert_eq!(FileUploadTool::mime_for_filename("a.png"), "image/png");
        assert_eq!(
            FileUploadTool::mime_for_filename("a.PDF"),
            "application/pdf"
        );
        assert_eq!(
            FileUploadTool::mime_for_filename("a.zip"),
            "application/zip"
        );
        assert_eq!(
            FileUploadTool::mime_for_filename("README.md"),
            "text/markdown"
        );
        assert_eq!(
            FileUploadTool::mime_for_filename("notes.markdown"),
            "text/markdown"
        );
        assert_eq!(FileUploadTool::mime_for_filename("a.txt"), "text/plain");
        assert_eq!(
            FileUploadTool::mime_for_filename("config.yaml"),
            "application/yaml"
        );
        assert_eq!(
            FileUploadTool::mime_for_filename("Cargo.toml"),
            "application/toml"
        );
        assert_eq!(
            FileUploadTool::mime_for_filename("app.js"),
            "application/javascript"
        );
        assert_eq!(
            FileUploadTool::mime_for_filename("report.xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(FileUploadTool::mime_for_filename("a.woff2"), "font/woff2");
        assert_eq!(
            FileUploadTool::mime_for_filename("noext"),
            "application/octet-stream"
        );
    }

    #[test]
    fn detect_mime_uses_content_sniff_for_binary_with_wrong_extension() {
        // PNG magic bytes — should win over the .tmp extension
        let png = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
        ];
        assert_eq!(
            FileUploadTool::detect_mime(&png, "screenshot.tmp"),
            "image/png"
        );

        // PDF magic bytes
        let pdf = b"%PDF-1.7\n";
        assert_eq!(
            FileUploadTool::detect_mime(pdf, "report.bin"),
            "application/pdf"
        );
    }

    #[test]
    fn detect_mime_falls_back_to_extension_for_text_formats() {
        // Markdown has no magic bytes; content-sniff returns None and we should
        // pick up the extension-table mapping.
        let md = b"# Title\n\nSome paragraph text.\n";
        assert_eq!(
            FileUploadTool::detect_mime(md, "README.md"),
            "text/markdown"
        );

        // YAML similarly has no magic bytes.
        let yaml = b"key: value\nother: 42\n";
        assert_eq!(
            FileUploadTool::detect_mime(yaml, "config.yaml"),
            "application/yaml"
        );
    }

    #[test]
    fn detect_mime_falls_back_to_octet_stream_for_unknown() {
        let bytes = b"\x00\x01\x02\x03unknown binary garbage";
        assert_eq!(
            FileUploadTool::detect_mime(bytes, "mystery.dat"),
            "application/octet-stream"
        );
    }

    #[test]
    fn truncate_response_body_passes_short_bodies_through() {
        assert_eq!(FileUploadTool::truncate_response_body("ok"), "ok");
        // Multi-byte but under the limit: returned unchanged.
        let small = "café ☕".to_string();
        assert_eq!(FileUploadTool::truncate_response_body(&small), small);
    }

    #[test]
    fn truncate_response_body_is_utf8_boundary_safe() {
        // '€' is 3 bytes and 4096 is not a multiple of 3, so the byte limit
        // lands inside a character — a naive `&body[..LIMIT]` slice would panic.
        let body = "€".repeat(2000); // 6000 bytes, well over the 4 KiB cap
        assert!(
            !body.is_char_boundary(RESPONSE_BODY_LIMIT_BYTES),
            "test precondition: limit must land mid-character"
        );

        let out = FileUploadTool::truncate_response_body(&body);

        // No panic, and the cut snaps down to the last whole char that fits:
        // floor(4096 / 3) = 1365 chars = 4095 bytes retained, 1905 truncated.
        assert!(out.contains("[truncated 1905 bytes]"), "got: {out}");
        assert!(out.starts_with("€".repeat(1365).as_str()));
        assert!(!out.starts_with("€".repeat(1366).as_str()));
    }

    #[tokio::test]
    async fn execute_truncates_multibyte_response_without_panicking() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        fs::write(&file, b"hello").unwrap();

        // Valid UTF-8 response whose 4 KiB cut point falls mid-character. Before
        // the boundary-safe truncation this panicked the tool path end to end.
        let big_body = "€".repeat(2000);
        Mock::given(method("POST"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(200).set_body_string(big_body))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileUploadConfig {
            url: Some(format!("{}/upload", server.uri())),
            ..FileUploadConfig::default()
        };
        let tool = FileUploadTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_path": "hello.txt" }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        assert!(
            result.output.contains("truncated"),
            "got: {}",
            result.output
        );
    }
}
