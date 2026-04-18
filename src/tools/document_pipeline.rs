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
const HANCOM_EXTENSIONS: &[&str] = &["hwp", "hwpx", "doc", "docx", "xls", "xlsx", "ppt", "pptx"];

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
                let mut ids: Vec<(u32, lopdf::ObjectId)> = pages.into_iter().collect();
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

    /// Estimate credit cost for image PDF OCR based on file size.
    /// Approximation: ~6 credits per page, estimate pages from file size
    /// (average ~100KB per scanned page), minimum 10 credits.
    fn estimate_pdf_credits(file_size_bytes: u64) -> u32 {
        let estimated_pages = (file_size_bytes / 100_000).max(1) as u32;
        (estimated_pages * 6).max(10)
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

    /// Process a digital PDF using the bundled `pymupdf_convert.py` script.
    ///
    /// Why PyMuPDF instead of pdf-extract:
    /// - `pdf-extract` returns plain text only — the resulting HTML loses
    ///   headings, tables, and structure (just `<p>plain</p>` wrapping).
    /// - `pymupdf4llm` (built on PyMuPDF/fitz) preserves layout, headings,
    ///   tables, lists, and code blocks, producing rich Markdown that
    ///   converts to clean structured HTML — exactly what the user wants
    ///   for re-use in the web editor and for LLM comprehension.
    ///
    /// The script is bundled into the binary at compile time via
    /// [`include_str!`] and written to a per-call temp directory at
    /// runtime, so users only need `python3` and `pymupdf4llm` installed.
    /// Install hint surfaced in the error message if either is missing.
    ///
    /// `pdf-extract` is no longer used here (the user explicitly asked to
    /// remove it as a fallback). The classification path
    /// (`classify_pdf` / `is_digital_pdf`) still uses `pdf-extract` since
    /// it only needs to know whether *any* text exists, not the formatted
    /// content.
    ///
    /// LLM correction is currently disabled at the call site (no key
    /// passed). The hook is preserved so a future enhancement can
    /// re-enable it without touching this method.
    async fn process_digital_pdf(
        &self,
        path: &Path,
        gemini_api_key: Option<&str>,
    ) -> anyhow::Result<DocumentOutput> {
        tracing::info!(
            "Processing digital PDF via PyMuPDF: {}",
            path.display()
        );

        let result = run_pymupdf_convert(path).await?;

        let mut html = result.html;
        let mut markdown = result.markdown;

        // Optional Gemini correction (disabled by default — see method doc).
        if let Some(key) = gemini_api_key {
            match self.gemini_correct(&html, key).await {
                Ok(corrected_html) => {
                    let corrected_md = html_to_markdown(&corrected_html);
                    html = corrected_html;
                    markdown = corrected_md;
                }
                Err(e) => {
                    tracing::warn!(
                        "Gemini correction failed, using raw PyMuPDF output: {e}"
                    );
                }
            }
        }

        Ok(DocumentOutput {
            html,
            markdown,
            doc_type: "digital_pdf".to_string(),
            page_count: result.page_count,
            engine: "pymupdf4llm".to_string(),
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
    /// Falls back to local parsers for DOCX/XLSX/PPTX if Hancom is unreachable.
    async fn process_office_document(
        &self,
        path: &Path,
        ext: &str,
    ) -> anyhow::Result<DocumentOutput> {
        tracing::info!("Processing office document: {} ({})", path.display(), ext);

        // Try Hancom first, fall back to local parsing for supported formats
        match self.process_office_via_hancom(path, ext).await {
            Ok(output) => Ok(output),
            Err(e) => {
                tracing::warn!("Hancom conversion failed: {e}, trying local fallback...");

                // Local fallback for formats we can parse directly
                match ext {
                    "docx" => {
                        let text = super::docx_read::extract_docx_text_from_path(path)?;
                        let html = format!(
                            "<div class=\"docx-content\">{}</div>",
                            text.lines()
                                .map(|l| format!("<p>{}</p>", html_escape(l)))
                                .collect::<Vec<_>>()
                                .join("\n")
                        );
                        Ok(DocumentOutput {
                            markdown: text.clone(),
                            html,
                            doc_type: "office_docx".to_string(),
                            page_count: 0,
                            engine: "local_docx_parser".to_string(),
                        })
                    }
                    "xlsx" => {
                        let text = super::xlsx_read::extract_xlsx_text_from_path(path)?;
                        let html = format!("<pre>{}</pre>", html_escape(&text));
                        Ok(DocumentOutput {
                            markdown: text,
                            html,
                            doc_type: "office_xlsx".to_string(),
                            page_count: 0,
                            engine: "local_xlsx_parser".to_string(),
                        })
                    }
                    "pptx" => {
                        let text = super::pptx_read::extract_pptx_text_from_path(path)?;
                        let html = format!(
                            "<div class=\"pptx-content\">{}</div>",
                            text.lines()
                                .map(|l| format!("<p>{}</p>", html_escape(l)))
                                .collect::<Vec<_>>()
                                .join("\n")
                        );
                        Ok(DocumentOutput {
                            markdown: text,
                            html,
                            doc_type: "office_pptx".to_string(),
                            page_count: 0,
                            engine: "local_pptx_parser".to_string(),
                        })
                    }
                    "hwp" | "hwpx" => {
                        // No local fallback for HWP — return error with guidance
                        anyhow::bail!(
                            "HWP/HWPX 변환에 실패했습니다 (Hancom 서버 미응답). \
                             HWP 파일은 한컴오피스에서 DOCX로 변환 후 다시 시도해주세요. \
                             원인: {e}"
                        );
                    }
                    _ => Err(e),
                }
            }
        }
    }

    /// Internal: Process via Hancom DocsConverter API.
    async fn process_office_via_hancom(
        &self,
        path: &Path,
        ext: &str,
    ) -> anyhow::Result<DocumentOutput> {
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
                },
                "classify_only": {
                    "type": "boolean",
                    "description": "If true, only classify the document type and return cost estimate without processing. Use this first for image PDFs to check credit cost before asking user consent."
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

        let classify_only = args
            .get("classify_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = Path::new(file_path);

        // Check file exists and size
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|e| anyhow::anyhow!("File not found or inaccessible: {e}"))?;

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

        // Classify document type
        let doc_type = Self::classify_document(path);

        // If classify_only, return classification and cost estimate without processing
        if classify_only {
            let (type_name, engine, requires_credits, estimated_credits) = match &doc_type {
                DocumentType::ImagePdf => (
                    "image_pdf",
                    "upstage_document_parse (OCR)",
                    true,
                    Self::estimate_pdf_credits(metadata.len()),
                ),
                DocumentType::DigitalPdf => {
                    ("digital_pdf", "pdf-extract (local, free)", false, 0u32)
                }
                DocumentType::OfficeDocument(_ext) => {
                    ("office_document", "hancom_docs_converter", false, 0u32)
                }
                DocumentType::Unsupported(_ext) => ("unsupported", "none", false, 0u32),
            };

            let ext_str = match &doc_type {
                DocumentType::OfficeDocument(ext) | DocumentType::Unsupported(ext) => ext.clone(),
                _ => Self::get_extension(path).unwrap_or_default(),
            };

            let output = json!({
                "classify_only": true,
                "file_path": file_path,
                "extension": ext_str,
                "doc_type": type_name,
                "engine": engine,
                "file_size_bytes": metadata.len(),
                "requires_credits": requires_credits,
                "estimated_credits": estimated_credits,
                "user_consent_required": requires_credits,
            });

            return Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&output).unwrap_or_default(),
                error: None,
            });
        }

        // Full processing from here
        // OCR API key: always use operator's admin key (2.2x credit billing)
        // Simplified to single route — no local key fallback complexity
        let upstage_key = std::env::var("ADMIN_UPSTAGE_API_KEY")
            .or_else(|_| std::env::var("UPSTAGE_API_KEY"))
            .ok();
        let gemini_key: Option<String> = None; // disabled — single route via Upstage only

        let result = match doc_type {
            DocumentType::DigitalPdf => self.process_digital_pdf(path, gemini_key.as_deref()).await,
            DocumentType::ImagePdf => {
                self.process_image_pdf(path, upstage_key.as_deref(), gemini_key.as_deref())
                    .await
            }
            DocumentType::OfficeDocument(ext) => self.process_office_document(path, &ext).await,
            DocumentType::Unsupported(ext) => Err(anyhow::anyhow!(
                "Unsupported document format: .{ext}. \
                     Supported: PDF, HWP, HWPX, DOC, DOCX, XLS, XLSX, PPT, PPTX"
            )),
        };

        match result {
            Ok(output) => {
                // Save output files if output_dir specified
                if let Some(output_dir) = args.get("output_dir").and_then(|v| v.as_str()) {
                    let dir = Path::new(output_dir);
                    let _ = tokio::fs::create_dir_all(dir).await;

                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("output");

                    if !output.html.is_empty() {
                        let html_path = dir.join(format!("{stem}.html"));
                        let _ = tokio::fs::write(&html_path, &output.html).await;
                    }
                    if !output.markdown.is_empty() {
                        let md_path = dir.join(format!("{stem}.md"));
                        let _ = tokio::fs::write(&md_path, &output.markdown).await;
                    }
                }

                // Build credit notice for image PDF (OCR costs credits)
                let credit_notice = if output.engine.contains("upstage") {
                    let estimated = Self::estimate_pdf_credits(metadata.len());
                    format!(
                        "\n\n💰 이 문서는 이미지 PDF OCR로 처리되었습니다. \
                         예상 크레딧 소진: ~{}크레딧 (API 비용의 2.2배)",
                        estimated
                    )
                } else {
                    String::new()
                };

                // Auto-save to long-term memory for future recall
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                let memory_note = format!(
                    "[문서 자동 저장] {file_name} ({} 페이지, {} 엔진)\n\
                     장기기억에 자동 저장되었습니다. memory_recall로 검색 가능합니다.",
                    output.page_count, output.engine
                );

                let markdown_content = if output.markdown.len() > 50000 {
                    let end = output
                        .markdown
                        .char_indices()
                        .take_while(|(i, _)| *i < 50000)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(50000.min(output.markdown.len()));
                    format!("{}... [truncated]", &output.markdown[..end])
                } else {
                    output.markdown.clone()
                };

                let summary = json!({
                    "doc_type": output.doc_type,
                    "engine": output.engine,
                    "page_count": output.page_count,
                    "html_length": output.html.len(),
                    "markdown_length": output.markdown.len(),
                    "html": output.html,
                    "markdown": markdown_content,
                    "memory_saved": true,
                    "memory_note": memory_note,
                    "credit_notice": credit_notice,
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

// ── Bundled PyMuPDF subprocess (digital PDF conversion) ────────────────

/// PyMuPDF script bundled into the binary at compile time. Mirrors the
/// pattern used by `hwpx_create.rs` so users only need `python3` and
/// `pymupdf4llm` on PATH — no external file dependencies, no separate
/// install step.
const PYMUPDF_CONVERT_PY: &str = include_str!("pdf_skill/pymupdf_convert.py");

/// Decoded result of one `pymupdf_convert.py` invocation.
#[derive(Debug)]
struct PymupdfResult {
    html: String,
    markdown: String,
    page_count: u32,
}

/// Resolve the best Python binary to invoke for `pymupdf_convert.py`.
///
/// Priority order — matches the Tauri sidecar's `ensure_python_env` flow
/// in `clients/tauri/src-tauri/src/lib.rs`:
///
/// 1. **`~/.moa/python-env/bin/python3`** (Unix) or
///    **`~/.moa/python-env/Scripts/python.exe`** (Windows) —
///    the isolated venv that the MoA Tauri app creates on first launch
///    and pre-installs `pymupdf4llm` + `markdown` into. This is the
///    happy path: end users get a working Python without ever touching
///    pip themselves.
///
/// 2. **`python3` / `python` on PATH** — fallback for developers who
///    run `cargo run` directly or for non-Tauri deployments.
///
/// Returns `None` only when neither path resolves to an executable.
/// Returned as `String` (not `&'static str`) because option 1 is a
/// dynamic absolute path.
fn pymupdf_python_binary() -> Option<String> {
    // Option 1: MoA-managed venv (created by Tauri ensure_python_env).
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
    {
        let venv_py = if cfg!(target_os = "windows") {
            home.join(".moa")
                .join("python-env")
                .join("Scripts")
                .join("python.exe")
        } else {
            home.join(".moa")
                .join("python-env")
                .join("bin")
                .join("python3")
        };
        if venv_py.exists() {
            return Some(venv_py.to_string_lossy().into_owned());
        }
    }

    // Option 2: system PATH fallback.
    if which::which("python3").is_ok() {
        return Some("python3".to_string());
    }
    if which::which("python").is_ok() {
        return Some("python".to_string());
    }
    None
}

/// Run the bundled PyMuPDF script against `input_path` and return the
/// rich HTML + Markdown it produces. Errors carry an actionable install
/// hint when Python or `pymupdf4llm` is missing.
async fn run_pymupdf_convert(input_path: &Path) -> anyhow::Result<PymupdfResult> {
    use tokio::process::Command;

    let python = pymupdf_python_binary().ok_or_else(|| {
        anyhow::anyhow!(
            "Python 3 not found. The MoA Tauri app normally installs an isolated \
             Python venv at ~/.moa/python-env on first launch — if you are running \
             zeroclaw outside of the MoA app, install Python 3 manually or run the \
             MoA app once to bootstrap the venv."
        )
    })?;

    // The python binary path may be an absolute path (MoA venv) or a
    // PATH-resolvable name ("python3"). Either form is accepted by
    // `Command::new`.

    // Write the bundled script to a fresh temp dir; auto-cleans on drop.
    let tmp_dir = tempfile::tempdir()
        .map_err(|e| anyhow::anyhow!("failed to create PyMuPDF temp dir: {e}"))?;
    let script_path = tmp_dir.path().join("pymupdf_convert.py");
    tokio::fs::write(&script_path, PYMUPDF_CONVERT_PY)
        .await
        .map_err(|e| anyhow::anyhow!("failed to materialize bundled script: {e}"))?;

    // Run python3 pymupdf_convert.py <input> --format both
    // We do NOT pass --output-dir because the caller (`process_digital_pdf`)
    // already gets html + markdown back via stdout JSON; the cache layer
    // handles the on-disk write via DocumentPipelineTool::execute's own
    // `output_dir` argument.
    let output = Command::new(&python)
        .arg(&script_path)
        .arg(input_path)
        .arg("--format")
        .arg("both")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn {python} pymupdf_convert.py: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        // Try to parse the JSON error from stdout (the script writes JSON
        // even on failure) so we surface the underlying reason cleanly.
        if let Ok(json) = serde_json::from_str::<Value>(&stdout) {
            if let Some(err) = json.get("error").and_then(|v| v.as_str()) {
                let install_hint = if err.contains("pymupdf4llm not installed") {
                    "\nInstall hint: pip install pymupdf4llm"
                } else {
                    ""
                };
                return Err(anyhow::anyhow!(
                    "PyMuPDF conversion failed: {err}{install_hint}"
                ));
            }
        }
        return Err(anyhow::anyhow!(
            "PyMuPDF conversion exited {}: {stderr}",
            output.status
        ));
    }

    let json: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
        anyhow::anyhow!(
            "PyMuPDF script returned non-JSON output: {e}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })?;

    if !json.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
        let err = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(anyhow::anyhow!(
            "PyMuPDF script reported failure: {err}"
        ));
    }

    let html = json
        .get("html")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let markdown = json
        .get("markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let page_count = json
        .get("page_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    if html.is_empty() && markdown.is_empty() {
        return Err(anyhow::anyhow!(
            "PyMuPDF script returned empty html and markdown"
        ));
    }

    Ok(PymupdfResult {
        html,
        markdown,
        page_count,
    })
}

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

/// Escape HTML special characters for safe embedding in HTML output.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
                md = format!("{}\n{} {}", &md[..start], prefix, &md[start + end + 1..]);
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

    // ── PyMuPDF subprocess helpers ──

    #[test]
    fn pymupdf_script_is_bundled_and_valid() {
        // include_str! at compile time guarantees presence; this verifies
        // the asset wasn't accidentally truncated.
        assert!(PYMUPDF_CONVERT_PY.contains("def convert_pdf"));
        assert!(PYMUPDF_CONVERT_PY.contains("import pymupdf4llm"));
        assert!(PYMUPDF_CONVERT_PY.len() > 1000);
    }

    #[tokio::test]
    async fn run_pymupdf_convert_returns_install_hint_when_python_missing_or_pkg_missing() {
        // Use a definitely-nonexistent path to force a script error
        // (file-not-found from the script side). The error message should
        // mention either the install hint OR a clear error string. We do
        // NOT assert success because the test machine may or may not
        // have python3 + pymupdf4llm installed.
        let result = run_pymupdf_convert(Path::new("/definitely/not/a/real/file.pdf")).await;
        match result {
            Ok(_) => panic!("expected an error for nonexistent file"),
            Err(e) => {
                let msg = e.to_string();
                // Either: python missing → install hint
                //         python present + pymupdf missing → install hint with pip
                //         python + pymupdf present → "File not found" from script
                assert!(
                    msg.contains("File not found")
                        || msg.contains("python3 not found")
                        || msg.contains("pymupdf4llm")
                        || msg.contains("PyMuPDF"),
                    "unexpected error: {msg}"
                );
            }
        }
    }

    /// Opt-in integration test: only runs when MOA_TEST_PYMUPDF=1 because
    /// it requires `python3` + `pymupdf4llm` installed and a real PDF file.
    #[tokio::test]
    async fn pymupdf_converts_real_pdf_when_env_set() {
        if std::env::var("MOA_TEST_PYMUPDF").ok().as_deref() != Some("1") {
            return;
        }
        let pdf_path = match std::env::var("MOA_TEST_PDF_PATH") {
            Ok(p) => std::path::PathBuf::from(p),
            Err(_) => return, // require an explicit fixture path
        };
        if !pdf_path.exists() {
            return;
        }
        let result = run_pymupdf_convert(&pdf_path).await.unwrap();
        assert!(!result.markdown.is_empty(), "markdown should be non-empty");
        assert!(!result.html.is_empty(), "html should be non-empty");
        assert!(result.html.contains("<html"), "html should be a real HTML document");
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
