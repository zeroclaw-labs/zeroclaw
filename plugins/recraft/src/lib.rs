//! ZeroClaw WASM plugin: image and SVG/vector generation via the Recraft API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! Recraft image-generation endpoint (OpenAI-images-compatible) accepts JSON and
//! returns a hosted image **URL** (a raster image, or an SVG when
//! `style="vector_illustration"`), so it works over the standard (text) host HTTP
//! bridge with no binary handling. Needs only the `http_client` and `env_read`
//! permissions.
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

const GENERATE_URL: &str = "https://external.api.recraft.ai/v1/images/generations";
const API_KEY_ENV: &str = "RECRAFT_API_KEY";
const DEFAULT_MODEL: &str = "recraftv3";

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

// ── Output formatting ─────────────────────────────────────────────

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present — `url` always,
/// `style` only when the caller requested one).
fn format_summary(style: Option<&str>, image_url: &str) -> String {
    let mut out = String::from("Generated image (Recraft).\n");
    let mut keys: Vec<&str> = Vec::new();
    if let Some(s) = style {
        out.push_str(&format!("Style: {s}\n"));
        keys.push("style");
    }
    out.push_str(&format!("Image URL: {image_url}\n"));
    keys.push("url");

    out.push_str("\n---\n");
    out.push_str("Data source: Recraft image-generation API (https://external.api.recraft.ai/v1/images/generations).\n");
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "image_generate".into(),
        description: "Generate an image, logo, or icon from a text prompt using Recraft. Set \
             style='vector_illustration' to produce native SVG/vector art (the only model that \
             does). Returns a hosted image URL."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing the image to generate."
                },
                "style": {
                    "type": "string",
                    "description": "Recraft style, e.g. 'realistic_image', 'digital_illustration', or 'vector_illustration' for SVG."
                },
                "size": {
                    "type": "string",
                    "description": "Image size, e.g. '1024x1024' (default), '1280x1024', '1024x1707'."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Recraft image generation tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse parameters ──────────────────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return fail("Missing required parameter: 'prompt'"),
    };
    let style = args
        .get("style")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let size = args
        .get("size")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    // ── Build the JSON body (only include provided optionals) ─────
    let mut body = serde_json::Map::new();
    body.insert("prompt".into(), json!(prompt));
    body.insert("model".into(), json!(DEFAULT_MODEL));
    body.insert("n".into(), json!(1));
    if let Some(s) = style {
        body.insert("style".into(), json!(s));
    }
    if let Some(s) = size {
        body.insert("size".into(), json!(s));
    }

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

    // ── Call Recraft via host HTTP function ───────────────────────
    let req = HttpRequest {
        method: "POST".into(),
        url: GENERATE_URL.into(),
        headers: [
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&serde_json::Value::Object(body))?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Recraft request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Recraft API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (image URL lives in data[0].url) ───────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Recraft response: {e}")))?;
    let url = match resp_json.pointer("/data/0/url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u,
        _ => return fail("Recraft response has no image URL"),
    };

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(style, url),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_lists_url_and_style() {
        let out = format_summary(Some("vector_illustration"), "https://img.test/a.svg");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Recraft image-generation API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("style"));
        assert!(line.contains("url"));
        assert!(body.contains("Style: vector_illustration"));
        assert!(body.contains("Image URL: https://img.test/a.svg"));
    }

    #[test]
    fn style_omitted_when_absent() {
        let out = format_summary(None, "https://img.test/a.png");
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(footer.contains("Fields returned: url."));
        assert!(!footer.contains("style"));
    }

    #[test]
    fn url_always_present() {
        let out = format_summary(None, "https://img.test/a.png");
        assert!(out.contains("Image URL: https://img.test/a.png"));
    }
}
