use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Maximum PPTX file size (50 MB).
const MAX_PPTX_BYTES: u64 = 50 * 1024 * 1024;
/// Default character limit returned to the LLM.
const DEFAULT_MAX_CHARS: usize = 50_000;
/// Hard ceiling regardless of what the caller requests.
const MAX_OUTPUT_CHARS: usize = 200_000;

/// Extract plain text from a PPTX file in the workspace.
pub struct PptxReadTool {
    security: Arc<SecurityPolicy>,
}

impl PptxReadTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

/// Extract plain text from PPTX bytes.
///
/// PPTX is a ZIP archive containing `ppt/slides/slide*.xml`.
/// Text lives inside `<a:t>` elements; paragraphs are delimited by `<a:p>`.
fn extract_pptx_text(bytes: &[u8]) -> anyhow::Result<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::io::Read;

    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    // Collect slide file names and sort for deterministic order.
    let mut slide_names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let name = archive.by_index(i).ok()?.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    slide_names.sort();

    if slide_names.is_empty() {
        anyhow::bail!("Not a valid PPTX (no slide XML files found)");
    }

    let mut text = String::new();

    for slide_name in &slide_names {
        let mut xml_content = String::new();
        archive
            .by_name(slide_name)
            .map_err(|e| anyhow::anyhow!("Failed to read {slide_name}: {e}"))?
            .read_to_string(&mut xml_content)?;

        let mut reader = Reader::from_str(&xml_content);
        let mut in_text = false;
        let slide_start = text.len();

        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    let name = e.name();
                    if name.as_ref() == b"a:t" {
                        in_text = true;
                    } else if name.as_ref() == b"a:p" && text.len() > slide_start {
                        text.push('\n');
                    }
                }
                Ok(Event::Empty(e)) => {
                    // Self-closing <a:t/> contains no text and must not flip `in_text`.
                    if e.name().as_ref() == b"a:p" && text.len() > slide_start {
                        text.push('\n');
                    }
                }
                Ok(Event::End(e)) => {
                    if e.name().as_ref() == b"a:t" {
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

        // Separate slides with a blank line.
        if text.len() > slide_start && !text.ends_with('\n') {
            text.push('\n');
        }
    }

    Ok(text)
}

#[async_trait]
impl Tool for PptxReadTool {
    fn name(&self) -> &str {
        "pptx_read"
    }

    fn description(&self) -> &str {
        "Extract plain text from a PPTX (PowerPoint) file in the workspace. \
         Returns all readable text content from all slides. No formatting, images, or charts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the PPTX file. Relative paths resolve from workspace."
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

        let full_path = self.security.workspace_dir.join(path);

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

        tracing::debug!("Reading PPTX: {}", resolved_path.display());

        match tokio::fs::metadata(&resolved_path).await {
            Ok(meta) => {
                if meta.len() > MAX_PPTX_BYTES {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "PPTX too large: {} bytes (limit: {MAX_PPTX_BYTES} bytes)",
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
                    error: Some(format!("Failed to read PPTX file: {e}")),
                });
            }
        };

        let text = match tokio::task::spawn_blocking(move || extract_pptx_text(&bytes)).await {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("PPTX extraction failed: {e}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("PPTX extraction task panicked: {e}")),
                });
            }
        };

        if text.trim().is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "PPTX contains no extractable text".into(),
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

    /// Build a minimal valid PPTX (ZIP) in memory with one slide containing the given text.
    fn minimal_pptx_bytes(slide_text: &str) -> Vec<u8> {
        use std::io::Write;

        let slide_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:txBody>
          <a:p><a:r><a:t>{slide_text}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#
        );

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        zip.start_file("ppt/slides/slide1.xml", options).unwrap();
        zip.write_all(slide_xml.as_bytes()).unwrap();

        let buf = zip.finish().unwrap();
        buf.into_inner()
    }

    /// Build a PPTX with two slides.
    fn two_slide_pptx_bytes(text1: &str, text2: &str) -> Vec<u8> {
        use std::io::Write;

        let make_slide = |text: &str| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:txBody>
          <a:p><a:r><a:t>{text}</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#
            )
        };

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        zip.start_file("ppt/slides/slide1.xml", options).unwrap();
        zip.write_all(make_slide(text1).as_bytes()).unwrap();

        zip.start_file("ppt/slides/slide2.xml", options).unwrap();
        zip.write_all(make_slide(text2).as_bytes()).unwrap();

        let buf = zip.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn name_is_pptx_read() {
        let tool = PptxReadTool::new(test_security(std::env::temp_dir()));
        assert_eq!(tool.name(), "pptx_read");
    }

    #[test]
    fn description_not_empty() {
        let tool = PptxReadTool::new(test_security(std::env::temp_dir()));
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_has_path_required() {
        let tool = PptxReadTool::new(test_security(std::env::temp_dir()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["max_chars"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = PptxReadTool::new(test_security(std::env::temp_dir()));
        let spec = tool.spec();
        assert_eq!(spec.name, "pptx_read");
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn missing_path_param_returns_error() {
        let tool = PptxReadTool::new(test_security(std::env::temp_dir()));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path"));
    }

    #[tokio::test]
    async fn absolute_path_is_blocked() {
        let tool = PptxReadTool::new(test_security(std::env::temp_dir()));
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
        let tool = PptxReadTool::new(test_security(tmp.path().to_path_buf()));
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
        let tool = PptxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "missing.pptx"})).await.unwrap();
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
        let tool = PptxReadTool::new(test_security_with_limit(tmp.path().to_path_buf(), 0));
        let result = tool.execute(json!({"path": "any.pptx"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn extracts_text_from_valid_pptx() {
        let tmp = TempDir::new().unwrap();
        let pptx_path = tmp.path().join("deck.pptx");
        tokio::fs::write(&pptx_path, minimal_pptx_bytes("Hello PPTX"))
            .await
            .unwrap();

        let tool = PptxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "deck.pptx"})).await.unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("Hello PPTX"),
            "expected extracted text, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn extracts_text_from_multiple_slides() {
        let tmp = TempDir::new().unwrap();
        let pptx_path = tmp.path().join("multi.pptx");
        tokio::fs::write(&pptx_path, two_slide_pptx_bytes("Slide One", "Slide Two"))
            .await
            .unwrap();

        let tool = PptxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "multi.pptx"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Slide One"));
        assert!(result.output.contains("Slide Two"));
    }

    #[tokio::test]
    async fn invalid_zip_returns_extraction_error() {
        let tmp = TempDir::new().unwrap();
        let pptx_path = tmp.path().join("bad.pptx");
        tokio::fs::write(&pptx_path, b"this is not a zip file")
            .await
            .unwrap();

        let tool = PptxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool.execute(json!({"path": "bad.pptx"})).await.unwrap();
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
        let long_text = "B".repeat(1000);
        let pptx_path = tmp.path().join("long.pptx");
        tokio::fs::write(&pptx_path, minimal_pptx_bytes(&long_text))
            .await
            .unwrap();

        let tool = PptxReadTool::new(test_security(tmp.path().to_path_buf()));
        let result = tool
            .execute(json!({"path": "long.pptx", "max_chars": 50}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("truncated"));
    }

    #[test]
    fn empty_text_tag_does_not_leak_in_text_state() {
        use std::io::Write;

        let slide_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:txBody>
          <a:p><a:r><a:t/></a:r></a:p>
          <a:p><a:r><a:t>Visible</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(buf);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("ppt/slides/slide1.xml", options).unwrap();
        zip.write_all(slide_xml.as_bytes()).unwrap();
        let bytes = zip.finish().unwrap().into_inner();

        let extracted = extract_pptx_text(&bytes).expect("extract text");
        assert!(extracted.contains("Visible"));
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
        tokio::fs::write(outside.join("secret.pptx"), minimal_pptx_bytes("secret"))
            .await
            .unwrap();
        symlink(outside.join("secret.pptx"), workspace.join("link.pptx")).unwrap();

        let tool = PptxReadTool::new(test_security(workspace));
        let result = tool.execute(json!({"path": "link.pptx"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("escapes workspace"));
    }
}
