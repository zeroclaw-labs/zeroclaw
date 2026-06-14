//! ZeroClaw WASM plugin: extract text/Markdown/HTML from Office documents.
//!
//! Pure byte transformer — no filesystem, network, or environment access.
//! The agent reads the file with the native `file_read` tool
//! (`encoding="base64"`), passes the base64 string here, and gets clean
//! text/Markdown back. All parsing happens inside the WASM sandbox.
//!
//! Input is bounded: the decoded payload is capped at [`MAX_DECODED_BYTES`]
//! (aligned with `file_read`'s own 10 MiB limit), checked before the base64
//! is decoded or handed to the parser, so the directly-invokable export cannot
//! be driven with an arbitrarily large or amplifying payload.
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `tool_metadata(_) -> JSON` — `{"name", "description", "parameters_schema"}`
//! - `execute(args_json) -> JSON` — `{"success", "output", "error?"}`
//!
//! No host functions are used and no permissions are requested.

// The `#[plugin_fn]` exports below only link against the Extism host on wasm.
// On the host (unit tests), they are cfg'd out, so the extism import and the
// metadata struct are unused there — allow that rather than restructure.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use base64::Engine;
#[cfg(target_arch = "wasm32")]
use extism_pdk::*;
use office_oxide::{Document, DocumentFormat};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Cursor;

#[derive(Serialize)]
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

const FORMATS: &[&str] = &["docx", "xlsx", "pptx", "doc", "xls", "ppt"];
const OUTPUTS: &[&str] = &["text", "markdown", "html"];

/// Maximum decoded document size. Aligned with the native `file_read` tool's
/// 10 MiB cap (`MAX_FILE_SIZE_BYTES`). The `file_read(base64) -> office_read`
/// pipeline is already bounded by that cap, but `office_read` is also directly
/// invokable, so this is the plugin-side contract that bounds input regardless
/// of the caller.
const MAX_DECODED_BYTES: usize = 10 * 1024 * 1024;

/// Maximum accepted base64 string length, derived from [`MAX_DECODED_BYTES`]
/// (4 encoded chars per 3 decoded bytes, plus padding slack). Checked before
/// the decode buffer is allocated so an oversize payload is rejected cheaply.
const MAX_ENCODED_LEN: usize = (MAX_DECODED_BYTES / 3 + 1) * 4;

fn parse_format(name: &str) -> Option<DocumentFormat> {
    match name.to_ascii_lowercase().as_str() {
        "docx" => Some(DocumentFormat::Docx),
        "xlsx" => Some(DocumentFormat::Xlsx),
        "pptx" => Some(DocumentFormat::Pptx),
        "doc" => Some(DocumentFormat::Doc),
        "xls" => Some(DocumentFormat::Xls),
        "ppt" => Some(DocumentFormat::Ppt),
        _ => None,
    }
}

/// Infer the document format from a filename extension.
fn format_from_filename(filename: &str) -> Option<DocumentFormat> {
    let ext = filename.rsplit('.').next()?;
    parse_format(ext)
}

/// Core extraction logic, kept independent of the plugin host so it can be
/// unit-tested directly. `execute` is a thin wrapper around this.
fn run(input: &str) -> ToolResult {
    let args: serde_json::Value = match serde_json::from_str(input) {
        Ok(v) => v,
        Err(e) => return ToolResult::failure(format!("Invalid JSON arguments: {e}")),
    };

    let content_b64 = match args.get("content_base64").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => return ToolResult::failure("Missing required parameter: 'content_base64'"),
    };

    // Size guard, before decode/parse. The direct-invocation path is not bounded
    // by file_read's 10 MiB cap, and even a small Office ZIP can expand into much
    // larger parser work, so oversize input is rejected up front. The encoded
    // check avoids allocating the decode buffer; the decoded check below is
    // defense-in-depth.
    if content_b64.len() > MAX_ENCODED_LEN {
        return ToolResult::failure(format!(
            "content_base64 too large: {} chars (limit {} chars, ~{} MiB decoded)",
            content_b64.len(),
            MAX_ENCODED_LEN,
            MAX_DECODED_BYTES / (1024 * 1024),
        ));
    }

    // Resolve the document format: explicit 'format' wins, else infer from 'filename'.
    let format = match args.get("format").and_then(|v| v.as_str()) {
        Some(f) if !f.trim().is_empty() => match parse_format(f.trim()) {
            Some(fmt) => fmt,
            None => {
                return ToolResult::failure(format!(
                    "Unsupported format '{f}'. Supported: {}",
                    FORMATS.join(", ")
                ));
            }
        },
        _ => match args.get("filename").and_then(|v| v.as_str()) {
            Some(name) => match format_from_filename(name) {
                Some(fmt) => fmt,
                None => {
                    return ToolResult::failure(format!(
                        "Could not infer format from filename '{name}'. \
                         Pass 'format' explicitly. Supported: {}",
                        FORMATS.join(", ")
                    ));
                }
            },
            None => {
                return ToolResult::failure(
                    "Provide 'format' or 'filename' so the document type can be determined.",
                );
            }
        },
    };

    let output_kind = args
        .get("output")
        .and_then(|v| v.as_str())
        .unwrap_or("markdown");
    if !OUTPUTS.contains(&output_kind) {
        return ToolResult::failure(format!(
            "Invalid output '{output_kind}'. Supported: {}",
            OUTPUTS.join(", ")
        ));
    }

    let bytes = match base64::engine::general_purpose::STANDARD.decode(content_b64) {
        Ok(b) => b,
        Err(e) => return ToolResult::failure(format!("Invalid base64 content: {e}")),
    };

    if bytes.len() > MAX_DECODED_BYTES {
        return ToolResult::failure(format!(
            "decoded document too large: {} bytes (limit {} bytes / {} MiB)",
            bytes.len(),
            MAX_DECODED_BYTES,
            MAX_DECODED_BYTES / (1024 * 1024),
        ));
    }

    let doc = match Document::from_reader(Cursor::new(bytes), format) {
        Ok(d) => d,
        Err(e) => return ToolResult::failure(format!("Failed to parse document: {e}")),
    };

    let output = match output_kind {
        "text" => doc.plain_text(),
        "html" => doc.to_html(),
        _ => doc.to_markdown(),
    };

    ToolResult::success(output)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "office_read".into(),
        description: "Extract plain text, Markdown, or HTML from an Office document \
                      (DOCX, XLSX, PPTX, DOC, XLS, PPT). Input is the file's raw bytes as a \
                      base64 string — NOT a path. \
                      REQUIRED usage: call this via the execute_pipeline tool, chaining \
                      file_read then office_read, so the base64 is passed machine-to-machine: \
                      step 0 = file_read {path, encoding:\"base64\"}; \
                      step 1 = office_read {content_base64:\"{{step[0].result}}\", filename, output:\"markdown\"}. \
                      Never write the base64 out yourself inline (it is huge and stalls generation), \
                      and never guess or fabricate the document's contents — if parsing fails, \
                      report the returned error. Pass 'filename' (with extension) or 'format' so \
                      the document type is detected. 'output': markdown (default), text, or html. \
                      Maximum document size is 10 MiB (decoded); larger inputs are rejected."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["content_base64"],
            "properties": {
                "content_base64": {
                    "type": "string",
                    "description": "The document's raw bytes, base64-encoded \
                                    (e.g. the output of file_read with encoding=\"base64\"). \
                                    Max 10 MiB decoded."
                },
                "format": {
                    "type": "string",
                    "enum": FORMATS,
                    "description": "Document format. If omitted, it is inferred from 'filename'."
                },
                "filename": {
                    "type": "string",
                    "description": "Original filename (e.g. 'report.docx'); used to infer \
                                    the format when 'format' is not given."
                },
                "output": {
                    "type": "string",
                    "enum": OUTPUTS,
                    "description": "Output representation (default: 'markdown')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    Ok(serde_json::to_string(&run(&input))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_content_is_rejected() {
        let r = run("{}");
        assert!(!r.success);
        assert!(r.error.unwrap().contains("content_base64"));
    }

    #[test]
    fn invalid_json_is_rejected() {
        let r = run("not json");
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Invalid JSON"));
    }

    #[test]
    fn unsupported_format_is_rejected() {
        let r = run(r#"{"content_base64":"aGk=","format":"rtf"}"#);
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Unsupported format"));
    }

    #[test]
    fn missing_format_and_filename_is_rejected() {
        let r = run(r#"{"content_base64":"aGk="}"#);
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Provide 'format' or 'filename'"));
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let r = run(r#"{"content_base64":"@@@not-base64@@@","format":"docx"}"#);
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Invalid base64"));
    }

    #[test]
    fn oversize_input_is_rejected_before_parsing() {
        // A base64 string longer than the encoded cap must be rejected up front,
        // before any decode or parse work happens.
        let huge = "A".repeat(MAX_ENCODED_LEN + 4);
        let input = format!(r#"{{"content_base64":"{huge}","format":"docx"}}"#);
        let r = run(&input);
        assert!(!r.success);
        assert!(r.error.unwrap().contains("too large"));
    }

    #[test]
    fn encoded_cap_matches_decoded_cap() {
        // The cheap encoded-length guard must bound the decoded size to <= the
        // documented decoded cap, so it cannot be bypassed.
        assert!(MAX_ENCODED_LEN / 4 * 3 <= MAX_DECODED_BYTES + 3);
    }
}
