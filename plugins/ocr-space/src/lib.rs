//! ZeroClaw WASM plugin: OCR text extraction from images and PDFs via OCR.space.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! caller passes a URL to an image or PDF; the plugin posts it to the OCR.space
//! parse endpoint and returns the extracted text. Uses host functions for the
//! outbound HTTP request and to read the API key, so it needs only the
//! `http_client` and `env_read` permissions.
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

const PARSE_URL: &str = "https://api.ocr.space/parse/image";
const API_KEY_ENV: &str = "OCR_SPACE_API_KEY";
const DEFAULT_LANGUAGE: &str = "eng";
const DEFAULT_ENGINE: u64 = 2;
/// Cap the returned text so a huge multi-page scan can't flood the context.
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

/// Percent-encode a value for an `application/x-www-form-urlencoded` body.
/// Only unreserved characters pass through untouched; everything else is
/// escaped, so an arbitrary image URL can be embedded safely.
fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── Output formatting (fidelity footer required) ──────────────────

/// Build the model-facing OCR summary plus the mandatory fidelity footer.
///
/// Every field shown is read directly from the OCR.space response; the footer
/// lists exactly those fields so the LLM cannot invent data (bounding boxes,
/// confidence, language, etc.) that OCR.space did not return.
fn format_summary(
    url: &str,
    text: &str,
    page_count: usize,
    exit_code: i64,
    processing_time_ms: &str,
    truncated: bool,
) -> String {
    let shown = if truncated {
        &text[..MAX_TEXT_CHARS]
    } else {
        text
    };
    let truncation_note = if truncated {
        format!(
            "\n\n[Text truncated to {MAX_TEXT_CHARS} characters of {} total.]",
            text.len()
        )
    } else {
        String::new()
    };

    let body = format!(
        "OCR result for: {url}\n\
         Pages: {page_count}, Exit code: {exit_code}, Processing time: {processing_time_ms} ms\n\
         \n\
         Text:\n{shown}{truncation_note}"
    );

    let footer = format!(
        "---\n\
         Data source: OCR.space OCR API ({PARSE_URL}).\n\
         Fields returned: url, text, page_count, exit_code, processing_time_ms.\n\
         Do not infer, estimate, or add fields that are not in this output."
    );

    format!("{body}\n\n{footer}")
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "ocr_space".into(),
        description:
            "Extract text (OCR) from an image or PDF located at a URL, via the OCR.space API. \
             Use this to read printed or scanned text from a picture, screenshot, or PDF \
             document when you only have its URL."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Direct URL to the image (PNG/JPG/GIF/TIF) or PDF to OCR."
                },
                "language": {
                    "type": "string",
                    "description": "Three-letter OCR language code (e.g. 'eng', 'spa', 'fre'). Default 'eng'."
                },
                "ocr_engine": {
                    "type": "integer",
                    "enum": [1, 2, 3],
                    "description": "OCR.space engine number: 1, 2 (default, better layout), or 3."
                },
                "is_table": {
                    "type": "boolean",
                    "description": "Set true for receipts/tables to preserve column layout. Default false."
                },
                "filetype": {
                    "type": "string",
                    "enum": ["PDF", "GIF", "PNG", "JPG", "TIF", "BMP"],
                    "description": "Override the file type when the URL has no recognizable extension."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the OCR.space text-extraction tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => return fail("Missing required parameter: 'url'"),
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return fail("Parameter 'url' must be an http(s) URL to an image or PDF");
    }

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_LANGUAGE);
    // Language codes are short alphabetic tokens; reject anything else so it
    // can't break out of the form body.
    if !language.chars().all(|c| c.is_ascii_alphabetic()) || language.len() > 8 {
        return fail(format!("Invalid language code '{language}'"));
    }

    let ocr_engine = args
        .get("ocr_engine")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_ENGINE);
    if !(1..=3).contains(&ocr_engine) {
        return fail(format!("Invalid ocr_engine '{ocr_engine}': use 1, 2, or 3"));
    }

    let is_table = args
        .get("is_table")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let filetype = args
        .get("filetype")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty());

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

    // ── Build the form body and call OCR.space ────────────────────
    let mut form = format!(
        "url={}&language={}&OCREngine={}&isTable={}&isOverlayRequired=false&scale=true",
        form_encode(&url),
        language,
        ocr_engine,
        is_table,
    );
    if let Some(ft) = &filetype {
        form.push_str(&format!("&filetype={ft}"));
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: PARSE_URL.into(),
        headers: [
            ("apikey".into(), api_key),
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
        ]
        .into_iter()
        .collect(),
        body: Some(form),
    };

    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("OCR.space request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "OCR.space API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ────────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse OCR.space response: {e}")))?;

    // OCR.space signals failure in-band with a 200 status.
    if resp_json
        .get("IsErroredOnProcessing")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let msg = error_message(&resp_json);
        return fail(format!("OCR.space could not process the file: {msg}"));
    }

    let exit_code = resp_json
        .get("OCRExitCode")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let processing_time_ms = resp_json
        .get("ProcessingTimeInMilliseconds")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let parsed_results = resp_json
        .get("ParsedResults")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let page_count = parsed_results.len();

    let text = parsed_results
        .iter()
        .filter_map(|r| r.get("ParsedText").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        return fail("OCR.space returned no text for this file");
    }

    let truncated = text.len() > MAX_TEXT_CHARS;
    let output = format_summary(
        &url,
        &text,
        page_count,
        exit_code,
        &processing_time_ms,
        truncated,
    );

    Ok(serde_json::to_string(&ToolResult::success(output))?)
}

/// OCR.space reports `ErrorMessage` as either a string or an array of strings.
fn error_message(resp: &serde_json::Value) -> String {
    match resp.get("ErrorMessage") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        _ => "unknown error".to_string(),
    }
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> String {
        format_summary(
            "https://example.com/receipt.png",
            "INVOICE\nTotal: $42.00",
            1,
            1,
            "359",
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
        assert!(out.contains("OCR result for: https://example.com/receipt.png"));
        assert!(out.contains("Pages: 1"));
        assert!(out.contains("Exit code: 1"));
        assert!(out.contains("Processing time: 359 ms"));
        assert!(out.contains("Total: $42.00"));
        assert!(
            out.contains("Fields returned: url, text, page_count, exit_code, processing_time_ms.")
        );
    }

    #[test]
    fn footer_is_last_in_the_output() {
        let out = sample_summary();
        let footer_pos = out.find("---").expect("footer present");
        let body_end = out.find("Text:").expect("body present");
        assert!(footer_pos > body_end, "footer must come after the body");
        assert!(
            out.trim_end().ends_with("not in this output."),
            "fidelity directive must be the final line"
        );
    }

    #[test]
    fn truncation_is_reported_in_body() {
        let big = "a".repeat(MAX_TEXT_CHARS + 100);
        let out = format_summary("https://e.test/x.pdf", &big, 3, 1, "1200", true);
        assert!(out.contains("[Text truncated to"));
        assert!(out.contains("\n---\n"));
        assert!(out.contains("Do not infer"));
    }

    #[test]
    fn form_encode_escapes_reserved_characters() {
        assert_eq!(
            form_encode("https://e.test/a b?c=1&d=2"),
            "https%3A%2F%2Fe.test%2Fa%20b%3Fc%3D1%26d%3D2"
        );
        // Unreserved characters pass through untouched.
        assert_eq!(form_encode("Az0-_.~"), "Az0-_.~");
    }

    #[test]
    fn error_message_handles_string_and_array() {
        let s = json!({ "ErrorMessage": "bad file" });
        assert_eq!(error_message(&s), "bad file");
        let a = json!({ "ErrorMessage": ["err one", "err two"] });
        assert_eq!(error_message(&a), "err one; err two");
    }
}
