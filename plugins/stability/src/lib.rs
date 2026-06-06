//! ZeroClaw WASM plugin: text-to-image generation via the Stability AI API.
//!
//! A stateless tool plugin — one request → one response, no stored state. Uses
//! the Stability v1 `text-to-image` endpoint, which accepts JSON and returns the
//! image as base64 *inside* JSON, so it works over the standard (text) host HTTP
//! bridge. The generated image is returned as an `image/png` data URI. Needs only
//! the `http_client` and `env_read` permissions.
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

const API_BASE: &str = "https://api.stability.ai/v1/generation/";
const API_KEY_ENV: &str = "STABILITY_API_KEY";
const DEFAULT_ENGINE: &str = "stable-diffusion-xl-1024-v1-0";

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

/// Build the model-facing output: a header, the image as a `image/png` data URI,
/// and the mandatory fidelity footer (last, naming the source and listing exactly
/// the fields present — `seed` is included only when the API returned one).
fn format_summary(engine: &str, seed: Option<u64>, image_b64: &str) -> String {
    let mut out = format!("Generated image.\nEngine: {engine}\n");
    let mut keys: Vec<&str> = vec!["engine"];
    if let Some(s) = seed {
        out.push_str(&format!("Seed: {s}\n"));
        keys.push("seed");
    }
    keys.push("image");
    out.push_str(&format!(
        "\nImage (data URI):\ndata:image/png;base64,{image_b64}"
    ));

    out.push_str("\n\n---\n");
    out.push_str(
        "Data source: Stability AI text-to-image API (https://api.stability.ai/v1/generation).\n",
    );
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
        description:
            "Generate an image from a text prompt using Stability AI (Stable Diffusion). Returns \
             the image as a PNG data URI. Optionally accepts a negative prompt and a specific \
             engine id."
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
                    "description": "Optional text describing what to avoid in the image."
                },
                "engine": {
                    "type": "string",
                    "description": "Stability v1 engine id (default 'stable-diffusion-xl-1024-v1-0')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Stability AI image generation tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return fail("Missing required parameter: 'prompt'"),
    };
    let negative_prompt = args
        .get("negative_prompt")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let engine = args
        .get("engine")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_ENGINE)
        .to_string();
    // engine id goes straight into the request URL path.
    if engine.contains("..")
        || engine.contains('?')
        || engine.contains('#')
        || engine.starts_with('/')
    {
        return fail(format!(
            "Invalid engine id '{engine}': must be a Stability engine id like 'stable-diffusion-xl-1024-v1-0'"
        ));
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

    // ── Build the JSON body ───────────────────────────────────────
    let mut text_prompts = vec![json!({ "text": prompt, "weight": 1.0 })];
    if let Some(neg) = negative_prompt {
        text_prompts.push(json!({ "text": neg, "weight": -1.0 }));
    }
    let body = json!({
        "text_prompts": text_prompts,
        "cfg_scale": 7,
        "height": 1024,
        "width": 1024,
        "samples": 1,
        "steps": 30
    });

    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{API_BASE}{engine}/text-to-image"),
        headers: [
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Stability request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Stability API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (base64 image lives in artifacts[0]) ───────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Stability response: {e}")))?;
    let artifact = match resp_json.pointer("/artifacts/0") {
        Some(a) => a,
        None => return fail("Stability returned no image artifacts"),
    };
    let image_b64 = match artifact.get("base64").and_then(|v| v.as_str()) {
        Some(b) if !b.is_empty() => b,
        _ => return fail("Stability artifact has no base64 image data"),
    };
    let seed = artifact.get("seed").and_then(|v| v.as_u64());

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&engine, seed, image_b64),
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
        let out = format_summary("sdxl", Some(42), "QUJD");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Stability AI text-to-image API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("engine"));
        assert!(line.contains("seed"));
        assert!(line.contains("image"));
        assert!(body.contains("Engine: sdxl"));
        assert!(body.contains("Seed: 42"));
        assert!(body.contains("data:image/png;base64,QUJD"));
    }

    #[test]
    fn seed_omitted_when_absent() {
        let out = format_summary("sdxl", None, "QUJD");
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("seed"));
        assert!(footer.contains("engine"));
        assert!(footer.contains("image"));
    }

    #[test]
    fn image_is_png_data_uri() {
        let out = format_summary("e", Some(1), "QUJD");
        assert!(out.contains("data:image/png;base64,QUJD"));
    }
}
