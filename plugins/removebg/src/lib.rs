//! ZeroClaw WASM plugin: remove an image background via the remove.bg API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! remove.bg endpoint takes a `multipart/form-data` request (with an `image_url`
//! field) and returns the cut-out image as **binary PNG**, so this plugin relies
//! on the host's base64 HTTP support (`body_base64` on the response, ZeroClaw
//! #7288): it reads the bytes back as base64 and returns an `image/png` data URI.
//! Needs only the `http_client` and `env_read` permissions.
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

const API_URL: &str = "https://api.remove.bg/v1.0/removebg";
const API_KEY_ENV: &str = "REMOVEBG_API_KEY";
const BOUNDARY: &str = "----ZeroClawRemoveBgBoundary7MA4YWxkTrZu0gW";

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

/// Mirrors the host response including the `body_base64` field (#7288) that
/// carries a binary response body.
#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_base64: Option<String>,
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

// ── Helpers ───────────────────────────────────────────────────────

/// Build a `multipart/form-data` body from text-only fields.
fn multipart_body(fields: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (name, value) in fields {
        body.push_str(&format!("--{BOUNDARY}\r\n"));
        body.push_str(&format!(
            "Content-Disposition: form-data; name=\"{name}\"\r\n\r\n"
        ));
        body.push_str(value);
        body.push_str("\r\n");
    }
    body.push_str(&format!("--{BOUNDARY}--\r\n"));
    body
}

/// Rough decoded byte count of a standard-base64 string (for a size note).
fn approx_bytes(b64: &str) -> usize {
    let pad = b64.bytes().rev().take_while(|&b| b == b'=').count();
    (b64.len() / 4) * 3 - pad
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(image_url: &str, image_b64: &str) -> String {
    let mut out = format!(
        "Removed background.\nSource: {image_url}\nResult size: ~{} bytes\n\nImage (data URI):\ndata:image/png;base64,{image_b64}",
        approx_bytes(image_b64)
    );
    out.push_str("\n\n---\n");
    out.push_str("Data source: remove.bg API (https://api.remove.bg/v1.0/removebg).\n");
    out.push_str("Fields returned: source_url, image.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "remove_background".into(),
        description:
            "Remove the background from an image using remove.bg. Provide the image URL; returns \
             the cut-out subject as a transparent PNG data URI."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["image_url"],
            "properties": {
                "image_url": {
                    "type": "string",
                    "description": "Absolute URL of the source image (http:// or https://)."
                },
                "size": {
                    "type": "string",
                    "description": "Output resolution: 'auto' (default), 'preview', or 'full'."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the remove.bg background-removal tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let image_url = match args.get("image_url").and_then(|v| v.as_str()) {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => return fail("Missing required parameter: 'image_url'"),
    };
    if !(image_url.starts_with("http://") || image_url.starts_with("https://")) {
        return fail(format!(
            "Invalid image_url '{image_url}': must be an absolute http:// or https:// URL"
        ));
    }
    let size = args
        .get("size")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("auto");

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

    // ── Call remove.bg (multipart request, binary response) ───────
    let body = multipart_body(&[("image_url", &image_url), ("size", size)]);
    let req = HttpRequest {
        method: "POST".into(),
        url: API_URL.into(),
        headers: [
            ("X-Api-Key".into(), api_key),
            (
                "Content-Type".into(),
                format!("multipart/form-data; boundary={BOUNDARY}"),
            ),
        ]
        .into_iter()
        .collect(),
        body: Some(body),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("remove.bg request failed: {e}")),
    };
    if resp.status >= 400 {
        // Errors come back as a JSON/text body, not an image.
        return fail(format!(
            "remove.bg API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Read the binary image (base64) ────────────────────────────
    let image_b64 = match resp.body_base64 {
        Some(b64) if !b64.is_empty() => b64,
        _ => {
            return fail(
                "remove.bg returned no binary image. The host may lack base64 HTTP response \
                 support (requires ZeroClaw #7288); upgrade the gateway/runtime to use this tool.",
            );
        }
    };

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&image_url, &image_b64),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_body_is_well_formed() {
        let b = multipart_body(&[("image_url", "https://e.test/a.png"), ("size", "auto")]);
        assert!(b.contains(&format!("--{BOUNDARY}\r\n")));
        assert!(b.contains(
            "Content-Disposition: form-data; name=\"image_url\"\r\n\r\nhttps://e.test/a.png\r\n"
        ));
        assert!(b.contains("name=\"size\"\r\n\r\nauto\r\n"));
        assert!(b.trim_end().ends_with(&format!("--{BOUNDARY}--")));
    }

    #[test]
    fn footer_present_last_lists_fields() {
        let out = format_summary("https://e.test/a.png", "QUJD");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: remove.bg API"));
        assert!(footer.contains("Fields returned: source_url, image."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Source: https://e.test/a.png"));
        assert!(body.contains("data:image/png;base64,QUJD"));
    }

    #[test]
    fn approx_bytes_handles_padding() {
        assert_eq!(approx_bytes("QUJD"), 3);
        assert_eq!(approx_bytes("QUI="), 2);
    }
}
