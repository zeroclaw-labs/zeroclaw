//! ZeroClaw WASM plugin: layout-aware OCR via the Mistral OCR API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! caller passes a URL to a document (PDF) or image; the plugin posts it to the
//! Mistral OCR endpoint and returns the recognized text as markdown, preserving
//! layout (headings, tables, lists). Uses host functions for the outbound HTTP
//! request and to read the API key, so it needs only the `http_client` and
//! `env_read` permissions.
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `tool_metadata(_) -> JSON` — returns `{"name", "description", "parameters_schema"}`
//! - `execute(args_json) -> JSON` — returns `{"success", "output", "error?"}`
//!
//! **Host functions (provided by the ZeroClaw runtime):**
//! - `zc_http_request(json) -> json` — make an HTTP request (`http_client` permission)
//! - `zc_env_read(name) -> value` — read an env var (`env_read` permission)

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

const OCR_URL: &str = "https://api.mistral.ai/v1/ocr";
const API_KEY_ENV: &str = "MISTRAL_API_KEY";
const DEFAULT_MODEL: &str = "mistral-ocr-latest";
/// Cap the returned markdown so a huge document can't flood the context.
/// When the OCR result exceeds this, the body is truncated and the footer says so.
const MAX_TEXT_CHARS: usize = 50_000;

// ── Types matching the host-side protocol ─────────────────────────

#[derive(Serialize, Deserialize)]
struct ToolMetadata {
    name: String,
    description: String,
    parameters_schema: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct ToolResult {
    success: bool,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ToolResult {
    fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
        }
    }
    fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
        }
    }
}

#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

// ── Host function declarations ────────────────────────────────────

#[host_fn]
extern "ExtismHost" {
    fn zc_http_request(input: String) -> String;
    fn zc_env_read(input: String) -> String;
}

fn http_request(req: &HttpRequest) -> Result<HttpResponse, Error> {
    let input = serde_json::to_string(req)?;
    let output = unsafe { zc_http_request(input)? };
    Ok(serde_json::from_str(&output)?)
}

fn env_read(var_name: &str) -> Result<String, Error> {
    unsafe { zc_env_read(var_name.to_string()) }
}

// ── Output formatting (fidelity footer required) ──────────────────

/// Build the model-facing OCR summary plus the mandatory fidelity footer.
///
/// Every field shown is read directly from the Mistral OCR response (except the
/// echoed input URL); the footer lists exactly those fields so the LLM cannot
/// invent data (bounding boxes, embedded image captions, confidence) that
/// Mistral did not return in this output.
fn format_summary(
    document_url: &str,
    model: &str,
    markdown: &str,
    page_count: usize,
    pages_processed: Option<i64>,
    truncated: bool,
) -> String {
    let shown = if truncated {
        &markdown[..MAX_TEXT_CHARS]
    } else {
        markdown
    };
    let truncation_note = if truncated {
        format!(
            "\n\n[Markdown truncated to {MAX_TEXT_CHARS} characters of {} total.]",
            markdown.len()
        )
    } else {
        String::new()
    };
    let processed_str = match pages_processed {
        Some(p) => p.to_string(),
        None => "not reported".to_string(),
    };

    let body = format!(
        "OCR result for: {document_url}\n\
         Model: {model}, Pages: {page_count}, Pages processed: {processed_str}\n\
         \n\
         Markdown:\n{shown}{truncation_note}"
    );

    let footer = format!(
        "---\n\
         Data source: Mistral OCR API ({OCR_URL}).\n\
         Fields returned: document_url, model, markdown, page_count, pages_processed.\n\
         Do not infer, estimate, or add fields that are not in this output."
    );

    format!("{body}\n\n{footer}")
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "mistral_ocr".into(),
        description:
            "Extract text from a document (PDF) or image at a URL as layout-aware markdown, \
             via the Mistral OCR API. Preserves structure like headings, tables, and lists. \
             Use this to read a scanned/printed document when you have its URL and want the \
             layout kept intact."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["document_url"],
            "properties": {
                "document_url": {
                    "type": "string",
                    "description": "Direct URL to the document (PDF) or image to OCR."
                },
                "document_type": {
                    "type": "string",
                    "enum": ["document_url", "image_url"],
                    "description": "Whether the URL points to a document/PDF ('document_url', default) or an image ('image_url')."
                },
                "model": {
                    "type": "string",
                    "description": "Mistral OCR model identifier. Default 'mistral-ocr-latest'."
                },
                "pages": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Optional zero-based page indices to OCR (e.g. [0, 1, 2]). Omit for all pages."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Mistral OCR tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let document_url = match args.get("document_url").and_then(|v| v.as_str()) {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => return fail("Missing required parameter: 'document_url'"),
    };
    if !(document_url.starts_with("http://") || document_url.starts_with("https://")) {
        return fail("Parameter 'document_url' must be an http(s) URL to a document or image");
    }

    let document_type = match args.get("document_type").and_then(|v| v.as_str()) {
        Some("image_url") => "image_url",
        Some("document_url") | None => "document_url",
        Some(other) => {
            return fail(format!(
                "Invalid document_type '{other}': use 'document_url' or 'image_url'"
            ));
        }
    };

    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL);

    // Optional explicit page selection — must be a JSON array of integers.
    let pages: Option<Vec<i64>> = match args.get("pages") {
        None => None,
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                match v.as_i64() {
                    Some(n) if n >= 0 => out.push(n),
                    _ => {
                        return fail("Parameter 'pages' must be an array of non-negative integers");
                    }
                }
            }
            Some(out)
        }
        Some(_) => return fail("Parameter 'pages' must be an array of integers"),
    };

    // ── Read API key via host function ────────────────────────────
    let api_key = match env_read(API_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => return fail(format!("API key {API_KEY_ENV} is empty")),
        Err(e) => {
            return fail(format!(
                "Missing API key: set the {API_KEY_ENV} environment variable ({e})"
            ));
        }
    };

    // ── Build the request body and call Mistral OCR ───────────────
    // Mistral keys the URL by its type: `document_url` for PDFs, `image_url`
    // for images. Build the object explicitly to use the dynamic key.
    let mut document = serde_json::Map::new();
    document.insert("type".into(), json!(document_type));
    document.insert(document_type.to_string(), json!(document_url));
    let document = serde_json::Value::Object(document);

    let mut payload = json!({
        "model": model,
        "document": document,
    });
    if let Some(p) = &pages {
        payload["pages"] = json!(p);
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: OCR_URL.into(),
        headers: [
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&payload)?),
    };

    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Mistral OCR request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Mistral OCR API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ────────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Mistral OCR response: {e}")))?;

    let resp_model = resp_json
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(model);

    let page_values = resp_json
        .get("pages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let page_count = page_values.len();

    let markdown = page_values
        .iter()
        .filter_map(|p| p.get("markdown").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
        .trim()
        .to_string();

    if markdown.is_empty() {
        return fail("Mistral OCR returned no text for this document");
    }

    let pages_processed = resp_json
        .pointer("/usage_info/pages_processed")
        .and_then(|v| v.as_i64());

    let truncated = markdown.len() > MAX_TEXT_CHARS;
    let output = format_summary(
        &document_url,
        resp_model,
        &markdown,
        page_count,
        pages_processed,
        truncated,
    );

    Ok(serde_json::to_string(&ToolResult::success(output))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> String {
        format_summary(
            "https://example.com/report.pdf",
            "mistral-ocr-latest",
            "# Title\n\nBody text.",
            2,
            Some(2),
            false,
        )
    }

    #[test]
    fn output_includes_fidelity_footer() {
        let out = sample_summary();
        assert!(out.contains("\n---\n"), "missing footer separator");
        assert!(out.contains("Data source:"), "missing data source line");
        assert!(
            out.contains("Fields returned:"),
            "missing fields-returned line"
        );
        assert!(out.contains("Do not infer"), "missing fidelity directive");
    }

    #[test]
    fn footer_lists_exactly_the_fields_in_the_body() {
        let out = sample_summary();
        assert!(out.contains("OCR result for: https://example.com/report.pdf"));
        assert!(out.contains("Model: mistral-ocr-latest"));
        assert!(out.contains("Pages: 2"));
        assert!(out.contains("Pages processed: 2"));
        assert!(out.contains("# Title"));
        assert!(out.contains(
            "Fields returned: document_url, model, markdown, page_count, pages_processed."
        ));
    }

    #[test]
    fn footer_is_last_in_the_output() {
        let out = sample_summary();
        let footer_pos = out.rfind("---").expect("footer present");
        let body_end = out.find("Markdown:").expect("body present");
        assert!(footer_pos > body_end, "footer must come after the body");
        assert!(
            out.trim_end().ends_with("not in this output."),
            "fidelity directive must be the final line"
        );
    }

    #[test]
    fn truncation_is_reported_in_body() {
        let big = "x".repeat(MAX_TEXT_CHARS + 50);
        let out = format_summary(
            "https://e.test/d.pdf",
            "mistral-ocr-latest",
            &big,
            9,
            Some(9),
            true,
        );
        assert!(out.contains("[Markdown truncated to"));
        assert!(out.contains("\n---\n"));
        assert!(out.contains("Do not infer"));
    }

    #[test]
    fn unreported_pages_processed_renders_safely() {
        let out = format_summary(
            "https://e.test/d.pdf",
            "mistral-ocr-latest",
            "text",
            1,
            None,
            false,
        );
        assert!(out.contains("Pages processed: not reported"));
        assert!(out.contains("\n---\n"));
        assert!(out.contains("Do not infer"));
    }
}
