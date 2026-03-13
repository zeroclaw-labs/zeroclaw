//! Document processing pipeline tool for MoA.
//!
//! Handles different document types with specialized processing:
//!
//! - **Image/Scanned PDF**: Upstage Document Parse (OCR) → Gemini Flash-Lite correction
//! - **Digital PDF**: Local text extraction (pdf-extract) → Gemini Flash-Lite correction
//! - **HWP/HWPX, DOC/DOCX, XLS/XLSX, PPT/PPTX**: Hancom DocsConverter API → HTML/Markdown
//!
//! ## Hybrid Architecture
//!
//! - **Operator API keys** (Upstage, Gemini) stay on the Railway server.
//! - **File uploads** go directly to external services via temporary tokens.
//! - **Control/billing** goes through Railway.
//!
//! ## Output
//!
//! All documents are converted to:
//! 1. **HTML** — displayed in WYSIWYG editor for user viewing/editing
//! 2. **Markdown** — fed to the AI for understanding and Q&A

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

/// File extensions supported by the Hancom DocsConverter API.
const HANCOM_EXTENSIONS: &[&str] = &[
    "hwp", "hwpx", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
];

/// Maximum file size for document processing (500MB).
const MAX_FILE_SIZE: u64 = 500 * 1024 * 1024;

/// Extension → Hancom module code mapping.
fn hancom_module_code(ext: &str) -> Option<&'static str> {
    match ext {
        "hwp" | "hwpx" => Some("hwp"),
        "doc" | "docx" => Some("word"),
        "xls" | "xlsx" => Some("cell"),
        "ppt" | "pptx" => Some("show"),
        _ => None,
    }
}

/// Maximum number of pages to sample when classifying a PDF.
const PDF_CLASSIFY_SAMPLE_PAGES: usize = 5;

/// Detect if a PDF is digital (has extractable text) vs image/scanned.
///
/// Uses a two-tier detection strategy for fast, precise classification:
///
/// 1. **Font resource check** (fast) — inspects `/Font` entries in sampled
///    page resources.  If no page carries a `/Font` resource the PDF is
///    almost certainly image-only.
/// 2. **Text extraction check** (confirmatory) — extracts text from the
///    full document via `pdf-extract` and verifies non-empty content.
///    This catches edge cases where font resources exist but contain no
///    renderable text (e.g. invisible watermark fonts).
///
/// Only PDFs with **no font resources AND no extractable text** are
/// classified as image PDFs and routed to Upstage OCR.  Already-OCR'd
/// scans (which carry a text layer) are correctly treated as digital.
#[cfg(feature = "rag-pdf")]
fn is_digital_pdf(path: &Path) -> bool {
    use std::fs;

    let data = match fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    // --- Tier 1: Font resource check (fast, no text parsing) ---
    let has_font = match lopdf::Document::load_mem(&data) {
        Ok(doc) => {
            let page_ids: Vec<_> = {
                let pages = doc.get_pages();
                let mut ids: Vec<(u32, lopdf::ObjectId)> =
                    pages.into_iter().collect();
                ids.sort_by_key(|(num, _)| *num);
                ids.into_iter().map(|(_, id)| id).collect()
            };

            let sample = sampled_indices(page_ids.len(), PDF_CLASSIFY_SAMPLE_PAGES);
            sample.iter().any(|&idx| {
                let page_id = page_ids[idx];
                page_has_font_resource(&doc, page_id)
            })
        }
        Err(e) => {
            tracing::debug!("lopdf failed to parse PDF for font check: {e}");
            // Cannot determine structure — fall through to text extraction.
            false
        }
    };

    if has_font {
        // Font resources present → digital (or already-OCR'd scan).
        return true;
    }

    // --- Tier 2: Text extraction check (confirmatory) ---
    // Even without /Font resources, attempt text extraction as a safety net.
    match pdf_extract::extract_text_from_mem(&data) {
        Ok(text) => !text.trim().is_empty(),
        Err(_) => false,
    }
}

/// Check whether a PDF page object carries a `/Font` resource.
#[cfg(feature = "rag-pdf")]
fn page_has_font_resource(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> bool {
    let page_obj = match doc.get_object(page_id) {
        Ok(obj) => obj,
        Err(_) => return false,
    };
    let dict = match page_obj.as_dict() {
        Ok(d) => d,
        Err(_) => return false,
    };

    // Try direct /Resources/Font on the page.
    if let Ok(resources) = dict.get(b"Resources") {
        if has_font_in_resources(doc, resources) {
            return true;
        }
    }

    // Walk /Parent chain — shared resources may live on a parent Pages node.
    let mut current = dict.get(b"Parent").ok().cloned();
    // Limit depth to prevent infinite loops on malformed PDFs.
    let mut depth = 0;
    while let Some(ref parent_ref) = current {
        if depth > 10 {
            break;
        }
        depth += 1;

        let parent_obj = match deref_object(doc, parent_ref) {
            Some(o) => o,
            None => break,
        };
        let parent_dict = match parent_obj.as_dict() {
            Ok(d) => d,
            Err(_) => break,
        };
        if let Ok(resources) = parent_dict.get(b"Resources") {
            if has_font_in_resources(doc, resources) {
                return true;
            }
        }
        current = parent_dict.get(b"Parent").ok().cloned();
    }

    false
}

/// Dereference an `Object::Reference` to the underlying object.
#[cfg(feature = "rag-pdf")]
fn deref_object<'a>(doc: &'a lopdf::Document, obj: &'a lopdf::Object) -> Option<&'a lopdf::Object> {
    match obj {
        lopdf::Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

/// Check whether a `/Resources` value (possibly an indirect reference)
/// contains a non-empty `/Font` dictionary.
#[cfg(feature = "rag-pdf")]
fn has_font_in_resources(doc: &lopdf::Document, resources: &lopdf::Object) -> bool {
    let res_obj = match deref_object(doc, resources) {
        Some(o) => o,
        None => return false,
    };
    let res_dict = match res_obj.as_dict() {
        Ok(d) => d,
        Err(_) => return false,
    };

    match res_dict.get(b"Font") {
        Ok(font_obj) => {
            let font = match deref_object(doc, font_obj) {
                Some(o) => o,
                None => return false,
            };
            match font.as_dict() {
                Ok(d) => !d.is_empty(),
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

/// Pick up to `max` evenly-spaced sample indices from a range of `total`.
///
/// Returns first, last, and evenly-distributed middle indices so that both
/// the beginning and end of a document are checked.
#[cfg(feature = "rag-pdf")]
fn sampled_indices(total: usize, max: usize) -> Vec<usize> {
    if total == 0 {
        return vec![];
    }
    if total <= max {
        return (0..total).collect();
    }
    let mut indices = Vec::with_capacity(max);
    for i in 0..max {
        let idx = i * (total - 1) / (max - 1);
        if indices.last() != Some(&idx) {
            indices.push(idx);
        }
    }
    indices
}

#[cfg(not(feature = "rag-pdf"))]
fn is_digital_pdf(_path: &Path) -> bool {
    false
}

/// Document processing pipeline tool.
pub struct DocumentPipelineTool {
    security: SecurityPolicy,
}

impl DocumentPipelineTool {
    pub fn new(security: SecurityPolicy) -> Self {
        Self { security }
    }

    /// Get the file extension (lowercase, without dot).
    fn get_extension(path: &Path) -> Option<String> {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
    }

    /// Determine document type and processing strategy.
    fn classify_document(path: &Path) -> DocumentType {
        let ext = Self::get_extension(path).unwrap_or_default();

        match ext.as_str() {
            "pdf" => {
                if is_digital_pdf(path) {
                    DocumentType::DigitalPdf
                } else {
                    DocumentType::ImagePdf
                }
            }
            e if HANCOM_EXTENSIONS.contains(&e) => DocumentType::OfficeDocument(ext),
            _ => DocumentType::Unsupported(ext),
        }
    }

    /// Process a digital PDF locally using pdf-extract (or PyMuPDF via sidecar).
    ///
    /// LLM correction is optional and only runs when the user has their own
    /// LLM API key configured. If no key, the raw extraction is returned as-is.
    async fn process_digital_pdf(
        &self,
        path: &Path,
        gemini_api_key: Option<&str>,
    ) -> anyhow::Result<DocumentOutput> {
        tracing::info!("Processing digital PDF: {}", path.display());

        // Extract text using pdf-extract (local, no API call needed)
        let text = {
            let path = path.to_owned();
            tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
                let data = std::fs::read(&path)?;
                #[cfg(feature = "rag-pdf")]
                {
                    let text = pdf_extract::extract_text_from_mem(&data)
                        .map_err(|e| anyhow::anyhow!("PDF text extraction failed: {e}"))?;
                    Ok(text)
                }
                #[cfg(not(feature = "rag-pdf"))]
                {
                    let _ = data;
                    anyhow::bail!("PDF extraction requires the 'rag-pdf' feature flag")
                }
            })
            .await??
        };

        // Convert plain text to basic HTML structure
        let html = text_to_html(&text);
        let markdown = text_to_markdown(&text);

        // Optional: Gemini Flash-Lite correction
        let (html, markdown) = if let Some(key) = gemini_api_key {
            match self.gemini_correct(&html, key).await {
                Ok(corrected_html) => {
                    let corrected_md = html_to_markdown(&corrected_html);
                    (corrected_html, corrected_md)
                }
                Err(e) => {
                    tracing::warn!("Gemini correction failed, using raw extraction: {e}");
                    (html, markdown)
                }
            }
        } else {
            (html, markdown)
        };

        Ok(DocumentOutput {
            html,
            markdown,
            doc_type: "digital_pdf".to_string(),
            page_count: 0, // Could be extracted with more sophisticated parsing
            engine: "pdf-extract".to_string(),
        })
    }

    /// Process an image/scanned PDF via Upstage Document Parse.
    ///
    /// **Local mode** (Tauri app): This is called when a user uploads an image PDF locally.
    /// The Upstage API key can come from the user's own settings.
    ///
    /// **Server mode** (Railway): Image PDFs go through the R2 pre-signed URL flow
    /// (`/api/document/upload-url` → `/api/document/process-r2`), NOT this method.
    ///
    /// LLM correction (Gemini/OpenAI/Claude) is optional and only runs when the
    /// user has configured their own LLM API key. If no LLM key is provided,
    /// the raw Upstage HTML/Markdown is returned as-is.
    async fn process_image_pdf(
        &self,
        path: &Path,
        upstage_api_key: Option<&str>,
        gemini_api_key: Option<&str>,
    ) -> anyhow::Result<DocumentOutput> {
        tracing::info!("Processing image/scanned PDF: {}", path.display());

        let api_key = upstage_api_key.ok_or_else(|| {
            anyhow::anyhow!(
                "Image PDF processing requires an Upstage API key. \
                 Set UPSTAGE_API_KEY in your environment or provide it in Settings."
            )
        })?;

        // Call Upstage Document Parse API
        let file_data = tokio::fs::read(path).await?;
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document.pdf");

        let client = reqwest::Client::new();
        let form = reqwest::multipart::Form::new()
            .part(
                "document",
                reqwest::multipart::Part::bytes(file_data)
                    .file_name(file_name.to_string())
                    .mime_str("application/pdf")?,
            )
            .text("model", "document-parse")
            .text("ocr", "force")
            .text("output_formats", "[\"html\"]")
            .text("coordinates", "true");

        let response = client
            .post("https://api.upstage.ai/v1/document-digitization")
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Upstage API error (HTTP {status}): {body}");
        }

        let data: Value = response.json().await?;

        // Extract HTML from Upstage response
        let html = data
            .get("content")
            .and_then(|c| c.get("html"))
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string();

        let page_count = data
            .get("elements")
            .and_then(|e| e.as_array())
            .map(|elements| {
                elements
                    .iter()
                    .filter_map(|e| e.get("page").and_then(|p| p.as_u64()))
                    .max()
                    .unwrap_or(1) as u32
            })
            .unwrap_or(1);

        // Gemini visual correction
        let html = if let Some(gemini_key) = gemini_api_key {
            match self.gemini_correct(&html, gemini_key).await {
                Ok(corrected) => corrected,
                Err(e) => {
                    tracing::warn!("Gemini correction failed: {e}");
                    html
                }
            }
        } else {
            html
        };

        let markdown = html_to_markdown(&html);

        Ok(DocumentOutput {
            html,
            markdown,
            doc_type: "image_pdf".to_string(),
            page_count,
            engine: "upstage_document_parse".to_string(),
        })
    }

    /// Process office documents (HWP, DOCX, XLSX, PPTX) via Hancom DocsConverter API.
    async fn process_office_document(
        &self,
        path: &Path,
        ext: &str,
    ) -> anyhow::Result<DocumentOutput> {
        tracing::info!("Processing office document: {} ({})", path.display(), ext);

        let module = hancom_module_code(ext).ok_or_else(|| {
            anyhow::anyhow!("Unsupported extension for Hancom DocsConverter: .{ext}")
        })?;

        // Load Hancom server configuration
        let host = std::env::var("HANCOM_HOST").unwrap_or_else(|_| "3.35.4.24".to_string());
        let port = std::env::var("HANCOM_PORT").unwrap_or_else(|_| "8101".to_string());
        let base_url = format!("http://{host}:{port}");

        let client = reqwest::Client::new();

        // Optional authentication
        let username = std::env::var("HANCOM_USERNAME").ok();
        let password = std::env::var("HANCOM_PASSWORD").ok();

        // Step 1: Upload file to Hancom server
        let upload_url = format!(
            "{}/upload",
            std::env::var("HANCOM_UPLOAD_URL").unwrap_or_else(|_| base_url.clone())
        );

        let file_data = tokio::fs::read(path).await?;
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document")
            .to_string();

        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(file_data)
                .file_name(file_name.clone())
                .mime_str("application/octet-stream")?,
        );

        let mut upload_req = client.post(&upload_url).multipart(form);

        // Add basic auth if configured
        if let (Some(user), Some(pass)) = (&username, &password) {
            upload_req = upload_req.basic_auth(user, Some(pass));
        }

        let upload_resp = upload_req
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;

        if !upload_resp.status().is_success() {
            let status = upload_resp.status();
            let body = upload_resp.text().await.unwrap_or_default();
            anyhow::bail!("Hancom file upload failed (HTTP {status}): {body}");
        }

        let upload_data: Value = upload_resp.json().await.unwrap_or(json!({}));
        let remote_path = upload_data
            .get("file_path")
            .or_else(|| upload_data.get("path"))
            .or_else(|| upload_data.get("filename"))
            .and_then(|v| v.as_str())
            .unwrap_or(&file_name);

        // Step 2: Call DocsConverter API
        let convert_url = format!(
            "{base_url}/{module}/doc2htm?file_path={remote_path}&show_type=0&function=sync"
        );

        let mut convert_req = client
            .get(&convert_url)
            .timeout(std::time::Duration::from_secs(300));

        if let (Some(user), Some(pass)) = (&username, &password) {
            convert_req = convert_req.basic_auth(user, Some(pass));
        }

        let convert_resp = convert_req.send().await?;

        if !convert_resp.status().is_success() {
            let status = convert_resp.status();
            let body = convert_resp.text().await.unwrap_or_default();
            anyhow::bail!("Hancom conversion failed (HTTP {status}): {body}");
        }

        let html = convert_resp.text().await?;
        let html = clean_hancom_html(&html);
        let markdown = html_to_markdown(&html);

        Ok(DocumentOutput {
            html,
            markdown,
            doc_type: format!("office_{ext}"),
            page_count: 0,
            engine: "hancom_docs_converter".to_string(),
        })
    }

    /// Call Gemini Flash-Lite for visual comparison and text correction.
    async fn gemini_correct(&self, html: &str, api_key: &str) -> anyhow::Result<String> {
        let client = reqwest::Client::new();
        let model = "gemini-2.0-flash-lite";
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
        );

        // Truncate HTML if too long for a single Gemini call.
        // Use floor_char_boundary to avoid panicking on multi-byte UTF-8 (e.g., Korean).
        let html_input = if html.len() > 30000 {
            let end = html
                .char_indices()
                .take_while(|(i, _)| *i < 30000)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(30000.min(html.len()));
            &html[..end]
        } else {
            html
        };

        let prompt = format!(
            r#"You are a document proofreader. Review and correct the following HTML extracted from a PDF document.

Fix ONLY:
1. OCR text errors (especially Korean spacing 띄어쓰기, Hanja confusion)
2. Heading levels that don't match visual hierarchy
3. Table structure issues (missing cells, wrong headers)
4. Number/digit position errors (MuPDF glyph width bug displaces digits)

Do NOT:
- Rewrite or restructure the document
- Remove or add sections
- Change the meaning of any content

Return the corrected HTML only, no explanations.

HTML to correct:
{html_input}"#
        );

        let body = json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "temperature": 0.1,
                "maxOutputTokens": 8192,
            }
        });

        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Gemini API error (HTTP {status}): {body}");
        }

        let data: Value = response.json().await?;
        let corrected = data
            .get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.get(0))
            .and_then(|p| p.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or(html)
            .to_string();

        // Strip markdown code fences if Gemini wrapped the output
        let trimmed = corrected.trim();
        let stripped = trimmed.strip_prefix("```html").unwrap_or(trimmed);
        let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
        let corrected = stripped.trim().to_string();

        Ok(corrected)
    }
}

#[async_trait]
impl Tool for DocumentPipelineTool {
    fn name(&self) -> &str {
        "document_process"
    }

    fn description(&self) -> &str {
        "Process documents (PDF, HWP, DOCX, XLSX, PPTX) into HTML and Markdown. \
         Auto-detects document type: digital PDF (local extraction), image PDF (Upstage OCR), \
         or office documents (Hancom converter). Returns HTML for WYSIWYG editing and \
         Markdown for AI understanding."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the document file to process"
                },
                "output_dir": {
                    "type": "string",
                    "description": "Optional directory to save output files (HTML/Markdown)"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        let path = Path::new(file_path);

        // Check file exists and size
        let metadata = tokio::fs::metadata(path).await.map_err(|e| {
            anyhow::anyhow!("File not found or inaccessible: {e}")
        })?;

        if metadata.len() > MAX_FILE_SIZE {
            return Ok(ToolResult {
                success: false,
                output: format!(
                    "File too large: {} bytes (max {} bytes)",
                    metadata.len(),
                    MAX_FILE_SIZE
                ),
                error: None,
            });
        }

        // Classify and process
        let doc_type = Self::classify_document(path);

        // Get API keys from environment (operator keys on Railway,
        // or user keys from local config)
        let upstage_key = std::env::var("UPSTAGE_API_KEY").ok();
        let gemini_key = std::env::var("GEMINI_API_KEY").ok();

        let result = match doc_type {
            DocumentType::DigitalPdf => {
                self.process_digital_pdf(path, gemini_key.as_deref())
                    .await
            }
            DocumentType::ImagePdf => {
                self.process_image_pdf(
                    path,
                    upstage_key.as_deref(),
                    gemini_key.as_deref(),
                )
                .await
            }
            DocumentType::OfficeDocument(ext) => {
                self.process_office_document(path, &ext).await
            }
            DocumentType::Unsupported(ext) => {
                Err(anyhow::anyhow!(
                    "Unsupported document format: .{ext}. \
                     Supported: PDF, HWP, HWPX, DOC, DOCX, XLS, XLSX, PPT, PPTX"
                ))
            }
        };

        match result {
            Ok(output) => {
                // Save output files if output_dir specified
                if let Some(output_dir) = args.get("output_dir").and_then(|v| v.as_str()) {
                    let dir = Path::new(output_dir);
                    let _ = tokio::fs::create_dir_all(dir).await;

                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");

                    if !output.html.is_empty() {
                        let html_path = dir.join(format!("{stem}.html"));
                        let _ = tokio::fs::write(&html_path, &output.html).await;
                    }
                    if !output.markdown.is_empty() {
                        let md_path = dir.join(format!("{stem}.md"));
                        let _ = tokio::fs::write(&md_path, &output.markdown).await;
                    }
                }

                let summary = json!({
                    "doc_type": output.doc_type,
                    "engine": output.engine,
                    "page_count": output.page_count,
                    "html_length": output.html.len(),
                    "markdown_length": output.markdown.len(),
                    "markdown": if output.markdown.len() > 50000 {
                        let end = output.markdown
                            .char_indices()
                            .take_while(|(i, _)| *i < 50000)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(50000.min(output.markdown.len()));
                        format!("{}... [truncated]", &output.markdown[..end])
                    } else {
                        output.markdown
                    },
                });

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&summary).unwrap_or_default(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Document processing failed: {e}"),
                error: Some(e.to_string()),
            }),
        }
    }
}

// ── Document types ──────────────────────────────────────────────

#[derive(Debug)]
enum DocumentType {
    DigitalPdf,
    ImagePdf,
    OfficeDocument(String),
    Unsupported(String),
}

#[derive(Debug)]
struct DocumentOutput {
    html: String,
    markdown: String,
    doc_type: String,
    page_count: u32,
    engine: String,
}

// ── HTML/Markdown conversion helpers ────────────────────────────

/// Convert plain text to basic HTML with paragraph structure.
fn text_to_html(text: &str) -> String {
    let mut html = String::from("<!DOCTYPE html><html><body>\n");
    for paragraph in text.split("\n\n") {
        let trimmed = paragraph.trim();
        if !trimmed.is_empty() {
            let escaped = trimmed
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            html.push_str(&format!("<p>{}</p>\n", escaped.replace('\n', "<br>")));
        }
    }
    html.push_str("</body></html>");
    html
}

/// Convert plain text to Markdown.
fn text_to_markdown(text: &str) -> String {
    // Already plain text — just ensure proper paragraph spacing
    let mut md = String::new();
    for paragraph in text.split("\n\n") {
        let trimmed = paragraph.trim();
        if !trimmed.is_empty() {
            md.push_str(trimmed);
            md.push_str("\n\n");
        }
    }
    md.trim_end().to_string()
}

/// Public wrapper for HTML→Markdown conversion used by gateway endpoints.
pub fn html_to_markdown_public(html: &str) -> String {
    html_to_markdown(html)
}

/// Convert HTML to Markdown (simplified conversion).
fn html_to_markdown(html: &str) -> String {
    let mut md = html.to_string();

    // Remove HTML/body tags
    md = md.replace("<!DOCTYPE html>", "");
    md = md.replace("<html>", "");
    md = md.replace("</html>", "");
    md = md.replace("<body>", "");
    md = md.replace("</body>", "");
    md = md.replace("<head>", "");
    md = md.replace("</head>", "");

    // Headings
    for level in (1..=6).rev() {
        let open = format!("<h{level}>");
        let close = format!("</h{level}>");
        let prefix = "#".repeat(level);
        md = md.replace(&open, &format!("\n{prefix} "));
        md = md.replace(&close, "\n");
    }

    // Also handle headings with attributes
    for level in (1..=6).rev() {
        let prefix = "#".repeat(level);
        let pattern = format!("<h{level} ");
        while let Some(start) = md.find(&pattern) {
            if let Some(end) = md[start..].find('>') {
                md = format!(
                    "{}\n{} {}",
                    &md[..start],
                    prefix,
                    &md[start + end + 1..]
                );
            } else {
                break;
            }
        }
    }

    // Paragraphs and breaks
    md = md.replace("<br>", "\n");
    md = md.replace("<br/>", "\n");
    md = md.replace("<br />", "\n");
    md = md.replace("<p>", "\n");
    md = md.replace("</p>", "\n");

    // Bold and italic
    md = md.replace("<strong>", "**");
    md = md.replace("</strong>", "**");
    md = md.replace("<b>", "**");
    md = md.replace("</b>", "**");
    md = md.replace("<em>", "*");
    md = md.replace("</em>", "*");
    md = md.replace("<i>", "*");
    md = md.replace("</i>", "*");

    // Lists
    md = md.replace("<ul>", "\n");
    md = md.replace("</ul>", "\n");
    md = md.replace("<ol>", "\n");
    md = md.replace("</ol>", "\n");
    md = md.replace("<li>", "- ");
    md = md.replace("</li>", "\n");

    // Strip remaining tags
    let mut result = String::new();
    let mut in_tag = false;
    for ch in md.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }

    // HTML entities
    result = result.replace("&amp;", "&");
    result = result.replace("&lt;", "<");
    result = result.replace("&gt;", ">");
    result = result.replace("&quot;", "\"");
    result = result.replace("&nbsp;", " ");

    // Clean up excessive whitespace
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result.trim().to_string()
}

/// Clean up Hancom DocsConverter HTML output.
fn clean_hancom_html(html: &str) -> String {
    let mut result = html.to_string();

    // Remove XML declaration
    if let Some(end) = result.find("?>") {
        if result.starts_with("<?xml") {
            result = result[end + 2..].trim_start().to_string();
        }
    }

    // Remove empty paragraphs and spans
    result = result.replace("<p></p>", "");
    result = result.replace("<span></span>", "");

    // Collapse multiple blank lines
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hancom_module_code() {
        assert_eq!(hancom_module_code("hwp"), Some("hwp"));
        assert_eq!(hancom_module_code("hwpx"), Some("hwp"));
        assert_eq!(hancom_module_code("docx"), Some("word"));
        assert_eq!(hancom_module_code("xlsx"), Some("cell"));
        assert_eq!(hancom_module_code("pptx"), Some("show"));
        assert_eq!(hancom_module_code("txt"), None);
    }

    #[test]
    fn test_text_to_html() {
        let text = "Hello world\n\nSecond paragraph";
        let html = text_to_html(text);
        assert!(html.contains("<p>Hello world</p>"));
        assert!(html.contains("<p>Second paragraph</p>"));
    }

    #[test]
    fn test_html_to_markdown() {
        let html = "<h1>Title</h1><p>Hello <strong>world</strong></p>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Title"));
        assert!(md.contains("**world**"));
    }

    #[test]
    fn test_classify_document() {
        assert!(matches!(
            DocumentPipelineTool::classify_document(Path::new("test.docx")),
            DocumentType::OfficeDocument(ext) if ext == "docx"
        ));
        assert!(matches!(
            DocumentPipelineTool::classify_document(Path::new("test.hwp")),
            DocumentType::OfficeDocument(ext) if ext == "hwp"
        ));
        assert!(matches!(
            DocumentPipelineTool::classify_document(Path::new("test.txt")),
            DocumentType::Unsupported(_)
        ));
    }

    // ── sampled_indices tests ──────────────────────────────────
    #[cfg(feature = "rag-pdf")]
    mod sampled_indices_tests {
        use super::super::sampled_indices;

        #[test]
        fn empty_total_returns_empty() {
            assert_eq!(sampled_indices(0, 5), Vec::<usize>::new());
        }

        #[test]
        fn total_within_max_returns_all() {
            assert_eq!(sampled_indices(3, 5), vec![0, 1, 2]);
            assert_eq!(sampled_indices(5, 5), vec![0, 1, 2, 3, 4]);
        }

        #[test]
        fn samples_include_first_and_last() {
            let s = sampled_indices(100, 5);
            assert_eq!(*s.first().unwrap(), 0);
            assert_eq!(*s.last().unwrap(), 99);
        }

        #[test]
        fn samples_are_sorted_and_unique() {
            let s = sampled_indices(1000, 5);
            for w in s.windows(2) {
                assert!(w[0] < w[1], "indices must be strictly increasing");
            }
        }

        #[test]
        fn single_page() {
            assert_eq!(sampled_indices(1, 5), vec![0]);
        }
    }
}
