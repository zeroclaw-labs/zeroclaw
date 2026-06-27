use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::FileUploadBundleConfig;

/// Read at most `limit` bytes from a response body via streaming,
/// then lossily convert to UTF-8. This avoids loading an unbounded
/// response into memory.
///
/// Returns the captured (lossy-UTF-8) body together with a
/// `was_truncated` flag that is `true` when reading stopped because the
/// byte limit was reached while more body remained. Callers must rely on
/// this flag rather than the captured length: a clean ASCII or otherwise
/// valid-UTF-8 body that overruns the limit is clipped to exactly `limit`
/// bytes, so its length alone is indistinguishable from a complete
/// response that happens to be exactly `limit` bytes long.
async fn read_response_bounded(response: reqwest::Response, limit: usize) -> (String, bool) {
    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut was_truncated = false;
    while let Some(chunk_result) = stream.next().await {
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(_) => break,
        };
        let remaining = limit.saturating_sub(buf.len());
        if remaining == 0 {
            // Buffer already full and another chunk arrived: the body
            // continues past the limit.
            was_truncated = true;
            break;
        }
        if chunk.len() > remaining {
            buf.extend_from_slice(&chunk[..remaining]);
            was_truncated = true;
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    (String::from_utf8_lossy(&buf).into_owned(), was_truncated)
}

/// Truncate a string to at most `limit` bytes without splitting a
/// multi-byte UTF-8 character.
fn truncate_utf8(s: &str, limit: usize) -> &str {
    if s.len() <= limit {
        return s;
    }
    // Walk backwards from the limit to find a valid char boundary.
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub struct FileUploadBundleTool {
    security: Arc<SecurityPolicy>,
    config: FileUploadBundleConfig,
}

impl FileUploadBundleTool {
    pub fn new(security: Arc<SecurityPolicy>, config: FileUploadBundleConfig) -> Self {
        Self { security, config }
    }

    fn mime_for_filename(name: &str) -> &'static str {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            // Images
            "png" | "apng" => "image/png",
            "jpg" | "jpeg" | "jfif" | "pjpeg" | "pjp" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "avif" => "image/avif",
            "bmp" => "image/bmp",
            "tiff" | "tif" => "image/tiff",
            "svg" => "image/svg+xml",
            "ico" => "image/vnd.microsoft.icon",
            "heic" | "heif" => "image/heic",
            "jxl" => "image/jxl",

            // Documents
            "pdf" => "application/pdf",
            "rtf" => "application/rtf",
            "epub" => "application/epub+zip",
            "doc" => "application/msword",
            "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "xls" => "application/vnd.ms-excel",
            "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "ppt" => "application/vnd.ms-powerpoint",
            "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "odt" => "application/vnd.oasis.opendocument.text",
            "ods" => "application/vnd.oasis.opendocument.spreadsheet",
            "odp" => "application/vnd.oasis.opendocument.presentation",

            // Structured data
            "json" => "application/json",
            "ndjson" | "jsonl" => "application/x-ndjson",
            "xml" => "application/xml",
            "yaml" | "yml" => "application/yaml",
            "toml" => "application/toml",
            "csv" => "text/csv",
            "tsv" => "text/tab-separated-values",
            "sql" => "application/sql",
            "ics" => "text/calendar",
            "vcf" => "text/vcard",

            // Text + markup
            "txt" | "log" | "ini" | "cfg" | "conf" | "env" => "text/plain",
            "md" | "markdown" => "text/markdown",
            "html" | "htm" => "text/html",
            "css" => "text/css",

            // Source code
            "js" | "mjs" | "cjs" => "application/javascript",
            "ts" | "tsx" => "application/typescript",
            "jsx" => "text/jsx",
            "py" => "text/x-python",
            "rb" => "text/x-ruby",
            "go" => "text/x-go",
            "rs" => "text/x-rust",
            "java" => "text/x-java",
            "kt" | "kts" => "text/x-kotlin",
            "swift" => "text/x-swift",
            "c" | "h" => "text/x-c",
            "cc" | "cpp" | "cxx" | "hpp" | "hh" => "text/x-c++",
            "cs" => "text/x-csharp",
            "sh" | "bash" | "zsh" => "application/x-sh",

            // Archives
            "zip" => "application/zip",
            "tar" => "application/x-tar",
            "gz" | "tgz" => "application/gzip",
            "bz2" | "tbz2" => "application/x-bzip2",
            "xz" | "txz" => "application/x-xz",
            "7z" => "application/x-7z-compressed",
            "rar" => "application/vnd.rar",

            // Audio
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "ogg" | "oga" | "opus" => "audio/ogg",
            "flac" => "audio/flac",
            "aac" => "audio/aac",
            "m4a" => "audio/mp4",
            "weba" => "audio/webm",
            "mid" | "midi" => "audio/midi",

            // Video
            "mp4" | "m4v" => "video/mp4",
            "webm" => "video/webm",
            "mov" | "qt" => "video/quicktime",
            "mkv" => "video/x-matroska",
            "avi" => "video/x-msvideo",
            "mpg" | "mpeg" => "video/mpeg",
            "3gp" => "video/3gpp",
            "3g2" => "video/3gpp2",

            // Fonts
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            "eot" => "application/vnd.ms-fontobject",

            // Web binary
            "wasm" => "application/wasm",

            _ => "application/octet-stream",
        }
    }
}

struct PreparedFile {
    file_name: String,
    bytes: Vec<u8>,
    mime: &'static str,
}

#[async_trait]
impl Tool for FileUploadBundleTool {
    fn name(&self) -> &str {
        "file_upload_bundle"
    }

    fn description(&self) -> &str {
        "Upload N local files as a single multipart/form-data request. \
         All files are sent in one HTTP round-trip; however, transactional \
         (all-or-nothing) semantics depend on the receiving endpoint. \
         Use for multi-file deliverables (HTML + CSS + JS, report + figures). \
         File paths stay on the host; bytes are not loaded into model context. \
         Returns the HTTP status and a truncated response body."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Paths to the files on the agent's filesystem. Relative paths resolve from the workspace."
                },
                "entry_file_name": {
                    "type": "string",
                    "description": "Optional filename within file_paths to mark as the bundle's entry (e.g. \"index.html\"). Defaults to the first file. Must match exactly one path's basename."
                },
                "project_id": {
                    "type": "string",
                    "description": "Optional project UUID to associate the bundle with on the receiver."
                }
            },
            "required": ["file_paths"]
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
                error: Some(
                    "file_upload_bundle is disabled: [file_upload_bundle].url is not configured"
                        .into(),
                ),
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

        let raw_paths = args
            .get("file_paths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::Error::msg("Missing 'file_paths' array parameter"))?;

        if raw_paths.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("file_paths must not be empty".into()),
            });
        }
        if raw_paths.len() as u64 > self.config.max_files as u64 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Too many files: {} (limit: {})",
                    raw_paths.len(),
                    self.config.max_files
                )),
            });
        }

        let entry_hint = args
            .get("entry_file_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let project_id = args
            .get("project_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let mut paths: Vec<String> = Vec::with_capacity(raw_paths.len());
        for (i, entry) in raw_paths.iter().enumerate() {
            let p = entry
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::Error::msg(format!("file_paths[{i}] must be a non-empty string"))
                })?;
            if !self.security.is_path_allowed(p) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Path not allowed by security policy: {p}")),
                });
            }
            paths.push(p.to_string());
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let mut prepared: Vec<PreparedFile> = Vec::with_capacity(paths.len());
        let mut seen_names: HashSet<String> = HashSet::with_capacity(paths.len());
        let mut total_bytes: u64 = 0;
        for path in &paths {
            let full_path = self.security.resolve_tool_path(path);

            let resolved_path: PathBuf = match tokio::fs::canonicalize(&full_path).await {
                Ok(p) => p,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to resolve file path {path}: {e}")),
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
                        error: Some(format!("Failed to read file metadata for {path}: {e}")),
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

            // Pre-check with metadata (cheap); the authoritative check
            // happens after the actual read to close the TOCTOU gap.
            if metadata.len() > self.config.max_file_size_bytes {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "File too large: {} is {} bytes (per-file limit: {} bytes)",
                        resolved_path.display(),
                        metadata.len(),
                        self.config.max_file_size_bytes
                    )),
                });
            }

            let file_name = resolved_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("upload")
                .to_string();
            if !seen_names.insert(file_name.clone()) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Duplicate file name in bundle: {file_name} (filenames must be unique)"
                    )),
                });
            }

            let bytes = match tokio::fs::read(&resolved_path).await {
                Ok(b) => b,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read {}: {e}", resolved_path.display())),
                    });
                }
            };

            // Authoritative size checks on the actual bytes read, closing
            // the TOCTOU window between metadata and read.
            let actual_len = bytes.len() as u64;
            if actual_len > self.config.max_file_size_bytes {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "File too large: {} is {} bytes (per-file limit: {} bytes)",
                        resolved_path.display(),
                        actual_len,
                        self.config.max_file_size_bytes
                    )),
                });
            }

            total_bytes = total_bytes.saturating_add(actual_len);
            if total_bytes > self.config.max_total_size_bytes {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Bundle too large: cumulative {} bytes exceeds limit {} bytes",
                        total_bytes, self.config.max_total_size_bytes
                    )),
                });
            }

            let mime = Self::mime_for_filename(&file_name);
            prepared.push(PreparedFile {
                file_name,
                bytes,
                mime,
            });
        }

        if let Some(name) = &entry_hint
            && !prepared.iter().any(|f| &f.file_name == name)
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "entry_file_name '{name}' does not match any file in file_paths"
                )),
            });
        }

        let mut form = reqwest::multipart::Form::new();
        for file in &prepared {
            let part = match reqwest::multipart::Part::bytes(file.bytes.clone())
                .file_name(file.file_name.clone())
                .mime_str(file.mime)
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
            form = form.part(self.config.field_name.clone(), part);
        }
        if let Some(name) = entry_hint {
            form = form.text("entry_file_name", name);
        }
        if let Some(pid) = project_id {
            form = form.text("project_id", pid);
        }

        let client = zeroclaw_config::schema::build_runtime_proxy_client_with_timeouts(
            "tool.file_upload_bundle",
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
                    error: Some(format!("Bundle upload request failed: {e}")),
                });
            }
        };

        let status = response.status();
        // Bounded streaming read — never buffers more than the limit.
        // `read_response_bounded` returns lossy UTF-8 so the result is
        // always a valid String, and `truncate_utf8` never splits a
        // multi-byte char. We gate the truncation marker on the reader's
        // `was_truncated` flag rather than the captured length, because a
        // clean ASCII/valid-UTF-8 body that overruns the limit is clipped
        // to exactly `body_limit` bytes and would otherwise be reported as
        // complete.
        let body_limit = self.config.max_response_body_bytes;
        let (raw_body, was_truncated) = read_response_bounded(response, body_limit).await;
        let truncated = if was_truncated {
            let safe = truncate_utf8(&raw_body, body_limit);
            format!("{safe}... [truncated]")
        } else {
            raw_body
        };

        let file_count = prepared.len();
        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: format!(
                    "Uploaded bundle of {file_count} files ({status}). Response: {truncated}"
                ),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: truncated,
                error: Some(format!(
                    "Upload endpoint returned status {status} for bundle of {file_count} files"
                )),
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

    fn cfg(url: Option<String>) -> FileUploadBundleConfig {
        FileUploadBundleConfig {
            url,
            ..FileUploadBundleConfig::default()
        }
    }

    #[test]
    fn tool_name_and_description() {
        let tmp = TempDir::new().unwrap();
        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );
        assert_eq!(tool.name(), "file_upload_bundle");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_requires_file_paths_array() {
        let tmp = TempDir::new().unwrap();
        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("file_paths".into())));
        assert_eq!(schema["properties"]["file_paths"]["type"], "array");
    }

    #[tokio::test]
    async fn execute_fails_when_url_unset() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("a.txt");
        fs::write(&file, b"a").unwrap();

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(None),
        );

        let result = tool
            .execute(json!({ "file_paths": ["a.txt"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn execute_blocks_readonly_autonomy() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("a.txt");
        fs::write(&file, b"a").unwrap();

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::ReadOnly),
            cfg(Some("https://example.com/upload_bundle".into())),
        );

        let result = tool
            .execute(json!({ "file_paths": ["a.txt"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_rejects_empty_file_paths() {
        let tmp = TempDir::new().unwrap();
        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );

        let result = tool.execute(json!({ "file_paths": [] })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("must not be empty"));
    }

    #[tokio::test]
    async fn execute_rejects_too_many_files() {
        let tmp = TempDir::new().unwrap();
        let mut config = cfg(Some("https://example.com/upload_bundle".into()));
        config.max_files = 2;
        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_paths": ["a.txt", "b.txt", "c.txt"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Too many files"));
    }

    #[tokio::test]
    async fn execute_rejects_per_file_over_size_cap() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("ok.bin"), vec![0u8; 100]).unwrap();
        fs::write(tmp.path().join("big.bin"), vec![0u8; 2048]).unwrap();

        let mut config = cfg(Some("https://example.com/upload_bundle".into()));
        config.max_file_size_bytes = 1024;

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_paths": ["ok.bin", "big.bin"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("too large"));
    }

    #[tokio::test]
    async fn execute_rejects_cumulative_over_total_cap() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.bin"), vec![0u8; 800]).unwrap();
        fs::write(tmp.path().join("b.bin"), vec![0u8; 800]).unwrap();

        let mut config = cfg(Some("https://example.com/upload_bundle".into()));
        config.max_file_size_bytes = 1024;
        config.max_total_size_bytes = 1024;

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_paths": ["a.bin", "b.bin"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Bundle too large"));
    }

    #[tokio::test]
    async fn execute_rejects_duplicate_filenames() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(tmp.path().join("index.html"), b"<a/>").unwrap();
        fs::write(sub.join("index.html"), b"<b/>").unwrap();

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );

        let result = tool
            .execute(json!({ "file_paths": ["index.html", "sub/index.html"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Duplicate file name"));
    }

    #[tokio::test]
    async fn execute_rejects_entry_not_in_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.html"), b"<a/>").unwrap();

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );

        let result = tool
            .execute(json!({
                "file_paths": ["a.html"],
                "entry_file_name": "missing.html"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("does not match any file"));
    }

    #[tokio::test]
    async fn execute_rejects_path_outside_workspace() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let file = outside.path().join("secret.txt");
        fs::write(&file, b"nope").unwrap();

        let tool = FileUploadBundleTool::new(
            test_security(workspace.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );

        let result = tool
            .execute(json!({ "file_paths": [file.to_string_lossy()] }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn execute_uploads_bundle_with_multipart_parts_and_headers() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("index.html"), b"<html></html>").unwrap();
        fs::write(tmp.path().join("styles.css"), b"body{}").unwrap();
        fs::write(tmp.path().join("app.js"), b"console.log(1)").unwrap();

        Mock::given(method("POST"))
            .and(path("/upload_bundle"))
            .and(header("X-Auth", "Bearer xyz"))
            .respond_with(ResponseTemplate::new(201).set_body_string(
                r#"{"bundle_id":"abc","entry_file_id":"def","files":[{"file_name":"index.html"},{"file_name":"styles.css"},{"file_name":"app.js"}]}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let mut headers = HashMap::new();
        headers.insert("X-Auth".into(), "Bearer xyz".into());
        let config = FileUploadBundleConfig {
            url: Some(format!("{}/upload_bundle", server.uri())),
            headers,
            ..FileUploadBundleConfig::default()
        };

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({
                "file_paths": ["index.html", "styles.css", "app.js"],
                "entry_file_name": "index.html",
                "project_id": "proj-42"
            }))
            .await
            .unwrap();

        assert!(result.success, "expected success, got {result:?}");
        assert!(result.output.contains("3 files"));
        assert!(result.output.contains("abc"));

        // Inspect the raw multipart body to verify all file parts and
        // optional text fields are present.
        let recorded = server
            .received_requests()
            .await
            .expect("wiremock should have captured the request");
        assert_eq!(recorded.len(), 1);
        let body = String::from_utf8_lossy(&recorded[0].body);

        // Each file part must appear with its Content-Disposition filename.
        for expected_name in ["index.html", "styles.css", "app.js"] {
            assert!(
                body.contains(&format!("filename=\"{expected_name}\"")),
                "multipart body should contain part for {expected_name}"
            );
        }
        // File content must be present in the body.
        assert!(body.contains("<html></html>"), "index.html content missing");
        assert!(body.contains("body{}"), "styles.css content missing");
        assert!(body.contains("console.log(1)"), "app.js content missing");

        // Text fields: entry_file_name and project_id.
        assert!(
            body.contains("entry_file_name") && body.contains("index.html"),
            "entry_file_name text field missing"
        );
        assert!(
            body.contains("project_id") && body.contains("proj-42"),
            "project_id text field missing"
        );
    }

    #[tokio::test]
    async fn execute_reports_non_2xx_response() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), b"a").unwrap();

        Mock::given(method("POST"))
            .and(path("/upload_bundle"))
            .respond_with(ResponseTemplate::new(422).set_body_string("bundle_too_large"))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileUploadBundleConfig {
            url: Some(format!("{}/upload_bundle", server.uri())),
            ..FileUploadBundleConfig::default()
        };

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_paths": ["a.txt"] }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("422"), "unexpected error: {err}");
    }

    #[test]
    fn mime_table_covers_common_bundle_extensions() {
        let cases = [
            // images
            ("photo.png", "image/png"),
            ("snap.JPG", "image/jpeg"),
            ("anim.gif", "image/gif"),
            ("hero.webp", "image/webp"),
            ("modern.avif", "image/avif"),
            ("favicon.ico", "image/vnd.microsoft.icon"),
            ("vector.svg", "image/svg+xml"),
            ("phone.heic", "image/heic"),
            // documents
            ("paper.PDF", "application/pdf"),
            (
                "brief.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            ),
            (
                "budget.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            (
                "slides.pptx",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            ),
            ("notes.odt", "application/vnd.oasis.opendocument.text"),
            ("book.epub", "application/epub+zip"),
            // data
            ("data.json", "application/json"),
            ("stream.ndjson", "application/x-ndjson"),
            ("conf.yaml", "application/yaml"),
            ("Cargo.toml", "application/toml"),
            ("rows.tsv", "text/tab-separated-values"),
            ("schema.sql", "application/sql"),
            ("invite.ics", "text/calendar"),
            // text + markup
            ("README.md", "text/markdown"),
            ("index.html", "text/html"),
            ("style.css", "text/css"),
            ("setup.env", "text/plain"),
            // source code
            ("app.js", "application/javascript"),
            ("api.ts", "application/typescript"),
            ("Page.tsx", "application/typescript"),
            ("main.py", "text/x-python"),
            ("lib.rs", "text/x-rust"),
            ("Main.kt", "text/x-kotlin"),
            ("run.sh", "application/x-sh"),
            ("app.cpp", "text/x-c++"),
            // archives
            ("src.zip", "application/zip"),
            ("logs.tar.gz", "application/gzip"),
            ("dump.bz2", "application/x-bzip2"),
            ("pack.7z", "application/x-7z-compressed"),
            // audio
            ("song.mp3", "audio/mpeg"),
            ("voice.flac", "audio/flac"),
            ("voice.m4a", "audio/mp4"),
            // video
            ("clip.mp4", "video/mp4"),
            ("rec.mkv", "video/x-matroska"),
            ("legacy.avi", "video/x-msvideo"),
            // fonts
            ("font.woff2", "font/woff2"),
            ("font.ttf", "font/ttf"),
            // web binary
            ("module.wasm", "application/wasm"),
            // fallback
            ("noext", "application/octet-stream"),
            ("weird.qq", "application/octet-stream"),
        ];
        for (name, expected) in cases {
            assert_eq!(
                FileUploadBundleTool::mime_for_filename(name),
                expected,
                "{name} should map to {expected}"
            );
        }
    }

    // ── truncate_utf8 ───────────────────────────────────────────

    #[test]
    fn truncate_utf8_within_limit() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
    }

    #[test]
    fn truncate_utf8_exact_boundary() {
        assert_eq!(truncate_utf8("hello", 5), "hello");
    }

    #[test]
    fn truncate_utf8_ascii() {
        assert_eq!(truncate_utf8("hello world", 5), "hello");
    }

    #[test]
    fn truncate_utf8_respects_char_boundary() {
        // "é" is 2 bytes (0xC3 0xA9). Cutting at byte 1 must back up.
        let s = "é";
        assert_eq!(s.len(), 2);
        assert_eq!(truncate_utf8(s, 1), "");
        assert_eq!(truncate_utf8(s, 2), "é");

        // "aé" = 3 bytes. Cutting at byte 2 must not split the é.
        let s2 = "aé";
        assert_eq!(truncate_utf8(s2, 2), "a");
        assert_eq!(truncate_utf8(s2, 3), "aé");
    }

    #[test]
    fn truncate_utf8_multibyte_emoji() {
        // "😀" is 4 bytes. Cutting at 1, 2, or 3 must produce "".
        let s = "😀";
        assert_eq!(s.len(), 4);
        assert_eq!(truncate_utf8(s, 1), "");
        assert_eq!(truncate_utf8(s, 2), "");
        assert_eq!(truncate_utf8(s, 3), "");
        assert_eq!(truncate_utf8(s, 4), "😀");
    }

    #[test]
    fn truncate_utf8_empty() {
        assert_eq!(truncate_utf8("", 0), "");
        assert_eq!(truncate_utf8("", 10), "");
    }

    // ── description wording ─────────────────────────────────────

    #[test]
    fn description_does_not_claim_atomicity() {
        let tmp = TempDir::new().unwrap();
        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            cfg(Some("https://example.com/upload_bundle".into())),
        );
        let desc = tool.description();
        // Must not promise all-or-nothing semantics.
        assert!(
            !desc.contains("All files land or none do"),
            "description should not claim atomic semantics"
        );
        assert!(
            !desc.contains("atomic"),
            "description should not use the word 'atomic'"
        );
    }

    // ── bounded response with multibyte boundary ────────────────

    #[tokio::test]
    async fn execute_truncates_over_limit_response_with_multibyte_boundary() {
        // Use a small response-body limit so we can craft a tight test
        // without allocating megabytes.
        let body_limit: usize = 64;

        // Build a response body that exceeds the limit and places a
        // multi-byte UTF-8 character ("é" = 2 bytes, 0xC3 0xA9) right
        // at the cut point so read_response_bounded slices mid-character.
        //
        // Layout: 63 bytes of ASCII padding + "é" (2 bytes) + more ASCII.
        // read_response_bounded reads the first 64 raw bytes, which cuts
        // the "é" after its first byte. from_utf8_lossy replaces the
        // dangling 0xC3 with U+FFFD (3 bytes), making the String 66
        // bytes — exceeding the 64-byte limit and triggering the
        // truncate_utf8 + "[truncated]" path.
        let padding = "A".repeat(63);
        let oversized_body = format!("{padding}é{}", "B".repeat(200));
        assert!(
            oversized_body.len() > body_limit,
            "test body must exceed limit"
        );

        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("payload.txt"), b"data").unwrap();

        Mock::given(method("POST"))
            .and(path("/upload_bundle"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oversized_body.clone()))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileUploadBundleConfig {
            url: Some(format!("{}/upload_bundle", server.uri())),
            max_response_body_bytes: body_limit,
            ..FileUploadBundleConfig::default()
        };

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_paths": ["payload.txt"] }))
            .await
            .expect("execute must not panic on multibyte boundary truncation");

        // The tool should succeed (HTTP 200) and the output must carry
        // the truncation marker.
        assert!(result.success, "expected success, got {result:?}");
        assert!(
            result.output.contains("[truncated]"),
            "output should contain [truncated] marker, got: {}",
            result.output
        );

        // The output (minus the "Uploaded bundle…" prefix and the
        // "... [truncated]" suffix) must be valid UTF-8 and must not
        // exceed the body limit.  We don't assert the exact byte count
        // because the prefix is implementation detail, but we verify the
        // response portion is bounded.
        let response_part = result
            .output
            .split("Response: ")
            .nth(1)
            .expect("output should contain 'Response: ' prefix");
        let before_marker = response_part
            .strip_suffix("... [truncated]")
            .expect("response part should end with '... [truncated]'");
        assert!(
            before_marker.len() <= body_limit,
            "truncated body ({} bytes) should not exceed limit ({} bytes)",
            before_marker.len(),
            body_limit,
        );
    }

    // ── bounded response with plain ASCII overrun ───────────────

    #[tokio::test]
    async fn execute_marks_over_limit_ascii_response_as_truncated() {
        // Regression: a clean ASCII (valid-UTF-8) response that overruns
        // the limit is clipped by read_response_bounded to exactly
        // `body_limit` bytes. The earlier `raw_body.len() > body_limit`
        // gate was then false, so the tool returned a clipped body with no
        // "[truncated]" marker — hiding from the agent that the receiver
        // body was cut. The reader's `was_truncated` flag must drive the
        // marker instead.
        let body_limit: usize = 64;

        // Pure ASCII, no multi-byte char near the cut point, so
        // from_utf8_lossy does not expand the captured bytes past the
        // limit (which is what masked the bug in the multibyte case).
        let oversized_body = "A".repeat(200);
        assert!(
            oversized_body.len() > body_limit,
            "test body must exceed limit"
        );

        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("payload.txt"), b"data").unwrap();

        Mock::given(method("POST"))
            .and(path("/upload_bundle"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oversized_body))
            .expect(1)
            .mount(&server)
            .await;

        let config = FileUploadBundleConfig {
            url: Some(format!("{}/upload_bundle", server.uri())),
            max_response_body_bytes: body_limit,
            ..FileUploadBundleConfig::default()
        };

        let tool = FileUploadBundleTool::new(
            test_security(tmp.path().to_path_buf(), AutonomyLevel::Full),
            config,
        );

        let result = tool
            .execute(json!({ "file_paths": ["payload.txt"] }))
            .await
            .expect("execute must not panic on ASCII truncation");

        assert!(result.success, "expected success, got {result:?}");
        assert!(
            result.output.contains("[truncated]"),
            "over-limit ASCII response must carry the [truncated] marker, got: {}",
            result.output
        );

        // The captured body must be bounded to exactly the limit (64 'A's)
        // with the marker following it.
        let response_part = result
            .output
            .split("Response: ")
            .nth(1)
            .expect("output should contain 'Response: ' prefix");
        let before_marker = response_part
            .strip_suffix("... [truncated]")
            .expect("response part should end with '... [truncated]'");
        assert_eq!(
            before_marker.len(),
            body_limit,
            "clipped ASCII body should be exactly the limit"
        );
        assert!(
            before_marker.bytes().all(|b| b == b'A'),
            "clipped body should be the leading 'A' run, got: {before_marker}"
        );
    }
}
