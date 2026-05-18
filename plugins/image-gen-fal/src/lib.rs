//! ZeroClaw WASM plugin: text-to-image generation via fal.ai Flux models.
//!
//! Mirrors the native `ImageGenTool` but runs as a sandboxed WASM plugin.
//! Uses host functions for HTTP requests and environment variable access.
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `tool_metadata(_) -> JSON` — returns `{"name", "description", "parameters_schema"}`
//! - `execute(args_json) -> JSON` — returns `{"success", "output", "error?"}`
//!
//! **Host functions (provided by ZeroClaw runtime):**
//! - `zc_http_request(json) -> json` — make an HTTP request (requires `http_client` permission)
//! - `zc_env_read(name) -> value` — read an env var (requires `env_read` permission)

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_MODEL: &str = "fal-ai/flux/schnell";
const DEFAULT_API_KEY_ENV: &str = "FAL_API_KEY";

const VALID_SIZES: &[&str] = &[
    "square_hd",
    "landscape_4_3",
    "portrait_4_3",
    "landscape_16_9",
    "portrait_16_9",
];

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
        Self { success: true, output: output.into(), error: None }
    }
    fn failure(error: impl Into<String>) -> Self {
        Self { success: false, output: String::new(), error: Some(error.into()) }
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

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "image_gen_fal".into(),
        description: "Generate an image from a text prompt using fal.ai (Flux models). \
                       Returns the image URL and metadata."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing the image to generate."
                },
                "size": {
                    "type": "string",
                    "enum": VALID_SIZES,
                    "description": "Image aspect ratio / size preset (default: 'square_hd')."
                },
                "model": {
                    "type": "string",
                    "description": "fal.ai model identifier (default: 'fal-ai/flux/schnell')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the image generation tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse parameters ──────────────────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return Ok(serde_json::to_string(&ToolResult::failure(
            "Missing required parameter: 'prompt'",
        ))?),
    };

    let size = args
        .get("size")
        .and_then(|v| v.as_str())
        .unwrap_or("square_hd");

    if !VALID_SIZES.contains(&size) {
        return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "Invalid size '{size}'. Valid values: {}",
            VALID_SIZES.join(", ")
        )))?);
    }

    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_MODEL);

    if model.contains("..")
        || model.contains('?')
        || model.contains('#')
        || model.contains('\\')
        || model.starts_with('/')
    {
        return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "Invalid model identifier '{model}'. \
             Must be a fal.ai model path (e.g. 'fal-ai/flux/schnell')."
        )))?);
    }

    // ── Read API key via host function ────────────────────────────
    let api_key = match env_read(DEFAULT_API_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "API key {DEFAULT_API_KEY_ENV} is empty"
        )))?),
        Err(e) => return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "Missing API key: set the {DEFAULT_API_KEY_ENV} environment variable ({e})"
        )))?),
    };

    // ── Call fal.ai via host HTTP function ────────────────────────
    let url = format!("https://fal.run/{model}");
    let body = json!({
        "prompt": prompt,
        "image_size": size,
        "num_images": 1
    });

    let req = HttpRequest {
        method: "POST".into(),
        url,
        headers: [
            ("Authorization".into(), format!("Key {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&body)?),
    };

    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "fal.ai request failed: {e}"
        )))?),
    };

    if resp.status >= 400 {
        return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "fal.ai API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        )))?);
    }

    // ── Parse response ───────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse fal.ai response: {e}")))?;

    let image_url = resp_json
        .pointer("/images/0/url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::msg("no image URL in fal.ai response"))?;

    Ok(serde_json::to_string(&ToolResult::success(format!(
        "Image generated successfully.\n\
         Model: {model}\n\
         Prompt: {prompt}\n\
         Image URL: {image_url}"
    )))?)
}
