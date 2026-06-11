//! ZeroClaw WASM plugin: local text-to-image via a self-hosted Stable Diffusion WebUI.
//!
//! A stateless tool plugin — one request → one response, no stored state. Targets
//! the user's own **local, keyless** AUTOMATIC1111 Stable Diffusion WebUI
//! (configurable base URL). Its `txt2img` endpoint returns the image as base64
//! *inside* JSON, so it works over the standard (text) host HTTP bridge — images
//! are generated on the user's own GPU, fully private, no third-party API. The
//! image is returned as a PNG data URI. Needs only the `http_client` and
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

/// Base URL of the self-hosted Stable Diffusion WebUI (AUTOMATIC1111).
const API_URL_ENV: &str = "SD_WEBUI_URL";
const DEFAULT_BASE: &str = "http://localhost:7860";

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

// ── Helpers ───────────────────────────────────────────────────────

/// Rough decoded byte count of a standard-base64 string (for a size note).
fn approx_bytes(b64: &str) -> usize {
    let pad = b64.bytes().rev().take_while(|&b| b == b'=').count();
    (b64.len() / 4) * 3 - pad
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
/// Slicing on a raw byte index (e.g. for an error body) can land inside a
/// multi-byte character and panic; this walks back to the nearest char boundary.
fn truncate_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Build the model-facing output (image as a data URI) and the mandatory
/// fidelity footer (last, naming the source and listing exactly the fields).
fn format_summary(prompt: &str, steps: u64, image_b64: &str) -> String {
    let mut out = format!(
        "Generated image (local Stable Diffusion WebUI).\nPrompt: {prompt}\nSteps: {steps}\nImage size: ~{} bytes\n\nImage (data URI):\ndata:image/png;base64,{image_b64}",
        approx_bytes(image_b64)
    );
    out.push_str("\n\n---\n");
    out.push_str("Data source: local Stable Diffusion WebUI (/sdapi/v1/txt2img).\n");
    out.push_str("Fields returned: prompt, steps, image.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "image_generate".into(),
        description:
            "Generate an image from a text prompt locally on your own Stable Diffusion WebUI \
             (AUTOMATIC1111) — fully private, no third-party API. Returns a PNG data URI. Set \
             SD_WEBUI_URL to your instance."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing the image to generate."
                },
                "negative_prompt": {
                    "type": "string",
                    "description": "Optional text describing what to avoid."
                },
                "steps": {
                    "type": "integer",
                    "description": "Sampling steps (1-150, default 20)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Stable Diffusion WebUI text-to-image tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse parameters ──────────────────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return fail("Missing required parameter: 'prompt'"),
    };
    let negative_prompt = args
        .get("negative_prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let steps = args
        .get("steps")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 150);

    // ── Resolve the local base URL (defaults to localhost) ────────
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your Stable Diffusion WebUI"
        ));
    }

    // ── Call the WebUI txt2img endpoint ───────────────────────────
    let body = json!({
        "prompt": prompt,
        "negative_prompt": negative_prompt,
        "steps": steps,
        "width": 512,
        "height": 512,
        "batch_size": 1
    });
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/sdapi/v1/txt2img"),
        headers: [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "Stable Diffusion WebUI request failed: {e}. Is it running at {base} with the API enabled (--api)?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "Stable Diffusion WebUI error ({}): {}",
            resp.status,
            truncate_chars(&resp.body, 500)
        ));
    }

    // ── Parse response (images[0] is base64 PNG) ──────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse WebUI response: {e}")))?;
    let image_b64 = match resp_json.pointer("/images/0").and_then(|v| v.as_str()) {
        Some(b) if !b.is_empty() => b,
        _ => return fail("Stable Diffusion WebUI returned no image"),
    };

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&prompt, steps, image_b64),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_lists_fields() {
        let out = format_summary("a cat", 20, "QUJD");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: local Stable Diffusion WebUI"));
        assert!(footer.contains("Fields returned: prompt, steps, image."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Prompt: a cat"));
        assert!(body.contains("Steps: 20"));
        assert!(body.contains("data:image/png;base64,QUJD"));
    }

    #[test]
    fn approx_bytes_handles_padding() {
        assert_eq!(approx_bytes("QUJD"), 3);
        assert_eq!(approx_bytes("QUI="), 2);
    }

    #[test]
    fn truncate_chars_never_splits_multibyte() {
        // A WebUI error body whose 500-byte cutoff lands mid-character must not
        // panic and must stay on a char boundary.
        let body = "é".repeat(400); // 800 bytes; boundary at 500 is mid-char
        let cut = truncate_chars(&body, 500);
        assert!(cut.len() <= 500);
        assert!(body.is_char_boundary(cut.len()));
        let msg = format!(
            "Stable Diffusion WebUI error ({}): {}",
            500,
            truncate_chars(&body, 500)
        );
        assert!(msg.starts_with("Stable Diffusion WebUI error (500):"));
    }

    #[test]
    fn truncate_chars_short_input_unchanged() {
        assert_eq!(truncate_chars("hello", 500), "hello");
        assert_eq!(truncate_chars("", 500), "");
    }
}
