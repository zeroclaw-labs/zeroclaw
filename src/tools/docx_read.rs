use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Maximum DOCX file size (50 MB).
const MAX_DOCX_BYTES: u64 = 50 * 1024 * 1024;
/// Default character limit returned to the LLM.
const DEFAULT_MAX_CHARS: usize = 50_000;
/// Hard ceiling regardless of what the caller requests.
const MAX_OUTPUT_CHARS: usize = 200_000;

/// Extract plain text from a DOCX file in the workspace.
pub struct DocxReadTool {
    security: Arc<SecurityPolicy>,
}

impl DocxReadTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

/// Extract plain text from DOCX bytes.
///
/// DOCX is a ZIP archive containing `word/document.xml`.
/// Text lives inside `<w:t>` elements; paragraphs are delimited by `<w:p>`.
fn extract_docx_text(bytes: &[u8]) -> anyhow::Result<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::io::Read;

    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    let mut xml_content = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|e| anyhow::anyhow!("Not a valid DOCX (missing word/document.xml): {e}"))?
        .read_to_string(&mut xml_content)?;

    let mut reader = Reader::from_str(&xml_content);
    let mut text = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                let name = e.name();
                if name.as_ref() == b"w:t" {
                    in_text = true;
                } else if name.as_ref() == b"w:p" && !text.is_empty() {
                    text.push('\n');
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"w:t" {
                    in_text = false;
                }
            }
            Ok(Event::Text(e)) => {
                if in_text {
                    text.push_str(&e.unescape()?);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    Ok(text)
}

#[async_trait]
impl Tool for DocxReadTool {
    fn name(&self) -> &str {
        "docx_read"
    }

    fn description(&self) -> &str {
        "Extract plain text from a DOCX (Word) file in the workspace. \
         Returns all readable text content. No formatting, images, or charts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the DOCX file. Relative paths resolve from workspace."
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 50000, max: 200000)",
                    "minimum": 1,
                    "maximum": 200_000
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' parameter"))?;

        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .map(|n| {
                usize::try_from(n)
                    .unwrap_or(MAX_OUTPUT_CHARS)
                    .min(MAX_OUTPUT_CHARS)
            })
            .unwrap_or(DEFAULT_MAX_CHARS);

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

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

        let full_path = self.security.resolve_user_supplied_path(path);

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

        tracing::debug!("Reading DOCX: {}", resolved_path.display());

        match tokio::fs::metadata(&resolved_path).await {
            Ok(meta) => {
                if meta.len() > MAX_DOCX_BYTES {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "DOCX too large: {} bytes (limit: {MAX_DOCX_BYTES} bytes)",
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

        let bytes = match tokio::fs::read(&resolved_path).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read DOCX file: {e}")),
                });
            }
        };

        let text = match tokio::task::spawn_blocking(move || extract_docx_text(&bytes)).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("DOCX extraction failed: {e}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("DOCX extraction task panicked: {e}")),
                });
            }
        };

        if text.trim().is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "DOCX contains no extractable text".into(),
                error: None,
            });
        }

        let output = if text.chars().count() > max_chars {
            let mut truncated: String = text.chars().take(max_chars).collect();
            use std::fmt::Write as _;
            let _ = write!(truncated, "\n\n... [truncated at {max_chars} chars]");
            truncated
        } else {
            text
        };

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
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_limit(
        workspace: std::path::PathBuf,
        max_actions: u32,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            max_actions_per_hour: max_actions,
            ..SecurityPolicy::default()
        })
    }

    /// Build a minimal valid DOCX (ZIP) in memory with the given text content.
    fn minimal_docx_bytes(body_text: &str) -> Vec<u8> {
        use std::io::Write;

        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>{body_text}</w:t></w:r></w:p>
  </w:body>
</w:document>"#
        );

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);

        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();

        let buf = zip.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn name_is_docx_read() {
        let tool = DocxReadTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "docx_read");
    }

    #[test]
    fn description_not_empty() {
        let tool = DocxReadTool::new(test_security(std::env::temp_dir()));
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_has_path_required() {
        let tool = DocxReadTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["max_chars"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = DocxReadTool::new(test_security(std::env::temp_dir()));
        let spec = tool.spec();
        assert_eq!(spec.name, "docx_read");
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn missing_path_param_returns_error() {
        let tool = DocxReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path"));
    }

    #[tokio::test]
    async fn absolute_path_is_blocked() {
        let tool = DocxReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({"path": "/etc/passwd"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    #[tokio::test]
    async fn path_traversal_is_blocked() {
        let tmp = TempDir::new().unwrap();
        let tool = DocxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "../../../etc/passwd"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not allowed"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let tmp = TempDir::new().unwrap();
        let tool = DocxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "does_not_exist.docx"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Failed to resolve"));
    }

    #[tokio::test]
    async fn rate_limit_blocks_request() {
        let tmp = TempDir::new().unwrap();
        let tool = DocxReadTool::new(test_security_with_limit(tmp.path().to_path_buf(), 0));
        let result = tool.execute(json!({"path": "any.docx"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn extracts_text_from_valid_docx() {
        let tmp = TempDir::new().unwrap();
        let docx_path = tmp.path().join("test.docx");
        tokio::fs::write(&docx_path, minimal_docx_bytes("Hello DOCX"))
            .await
            .unwrap();

        let tool = DocxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "test.docx"})).await.unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("Hello DOCX"),
            "expected extracted text, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn invalid_zip_returns_extraction_error() {
        let tmp = TempDir::new().unwrap();
        let docx_path = tmp.path().join("bad.docx");
        tokio::fs::write(&docx_path, b"this is not a zip file")
            .await
            .unwrap();

        let tool = DocxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "bad.docx"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("extraction failed"));
    }

    #[tokio::test]
    async fn max_chars_truncates_output() {
        let tmp = TempDir::new().unwrap();
        let long_text = "A".repeat(1000);
        let docx_path = tmp.path().join("long.docx");
        tokio::fs::write(&docx_path, minimal_docx_bytes(&long_text))
            .await
            .unwrap();

        let tool = DocxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "long.docx", "max_chars": 50}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("truncated"));
    }

    #[test]
    fn extract_docx_text_multiple_paragraphs() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>First</w:t></w:r></w:p>
    <w:p><w:r><w:t>Second</w:t></w:r></w:p>
  </w:body>
</w:document>"#;

        use std::io::Write;
        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        let buf = zip.finish().unwrap();

        let text = extract_docx_text(&buf.into_inner()).unwrap();
        assert!(text.contains("First"));
        assert!(text.contains("Second"));
        assert!(
            text.contains('\n'),
            "paragraphs should be separated by newline"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_is_blocked() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("outside");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("secret.docx"), minimal_docx_bytes("secret"))
            .await
            .unwrap();
        symlink(outside.join("secret.docx"), workspace.join("link.docx")).unwrap();

        let tool = DocxReadTool::new(test_security(workspace));
        let result = tool.execute(json!({"path": "link.docx"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("escapes workspace"));
    }
}
