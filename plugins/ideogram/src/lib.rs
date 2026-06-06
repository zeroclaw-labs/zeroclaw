//! ZeroClaw WASM plugin: text-to-image with accurate in-image text via Ideogram.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! Ideogram generate endpoint accepts JSON and returns a hosted image **URL**,
//! so it works over the standard (text) host HTTP bridge with no binary handling.
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

const GENERATE_URL: &str = "https://api.ideogram.ai/generate";
const API_KEY_ENV: &str = "IDEOGRAM_API_KEY";

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

/// A field actually returned by Ideogram. The body and the fidelity footer
/// derive from the same set so the footer can't claim an absent field.
struct Field {
    key: &'static str,
    label: &'static str,
    value: String,
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present — `url` always).
fn format_summary(image_url: &str, fields: &[Field]) -> String {
    let mut out = String::from("Generated image (Ideogram).\n");
    for f in fields {
        out.push_str(&format!("{}: {}\n", f.label, f.value));
    }
    out.push_str(&format!("Image URL: {image_url}\n"));

    let mut keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
    keys.push("url");
    out.push_str("\n---\n");
    out.push_str("Data source: Ideogram generate API (https://api.ideogram.ai/generate).\n");
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
            "Generate an image from a text prompt using Ideogram, which excels at rendering \
             accurate text inside images (signs, logos, posters). Returns a hosted image URL."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text prompt describing the image (include any text to render in quotes)."
                },
                "aspect_ratio": {
                    "type": "string",
                    "description": "Aspect ratio, e.g. 'ASPECT_1_1', 'ASPECT_16_9', 'ASPECT_9_16' (default 1:1)."
                },
                "style_type": {
                    "type": "string",
                    "description": "Style: 'GENERAL', 'REALISTIC', 'ANIME', 'DESIGN', or 'RENDER_3D'."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Ideogram image generation tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return fail("Missing required parameter: 'prompt'"),
    };

    // ── Build image_request, including only provided optionals ────
    let mut image_request = serde_json::Map::new();
    image_request.insert("prompt".into(), json!(prompt));
    image_request.insert("num_images".into(), json!(1));
    if let Some(ar) = args.get("aspect_ratio").and_then(|v| v.as_str())
        && !ar.trim().is_empty()
    {
        image_request.insert("aspect_ratio".into(), json!(ar.trim()));
    }
    if let Some(st) = args.get("style_type").and_then(|v| v.as_str())
        && !st.trim().is_empty()
    {
        image_request.insert("style_type".into(), json!(st.trim()));
    }
    let body = json!({ "image_request": serde_json::Value::Object(image_request) });

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

    // ── Call Ideogram via host HTTP function ──────────────────────
    let req = HttpRequest {
        method: "POST".into(),
        url: GENERATE_URL.into(),
        headers: [
            ("Api-Key".into(), api_key),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Ideogram request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Ideogram API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (image URL lives in data[0]) ───────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Ideogram response: {e}")))?;
    let item = match resp_json.pointer("/data/0") {
        Some(i) => i,
        None => return fail("Ideogram returned no image data"),
    };
    let url = match item.get("url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u,
        _ => return fail("Ideogram response has no image URL"),
    };

    let mut fields: Vec<Field> = Vec::new();
    if let Some(res) = item.get("resolution").and_then(|v| v.as_str()) {
        fields.push(Field {
            key: "resolution",
            label: "Resolution",
            value: res.to_string(),
        });
    }
    if let Some(seed) = item.get("seed").and_then(|v| v.as_u64()) {
        fields.push(Field {
            key: "seed",
            label: "Seed",
            value: seed.to_string(),
        });
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(url, &fields),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_lists_url() {
        let fields = vec![
            Field {
                key: "resolution",
                label: "Resolution",
                value: "1024x1024".into(),
            },
            Field {
                key: "seed",
                label: "Seed",
                value: "7".into(),
            },
        ];
        let out = format_summary("https://img.test/a.png", &fields);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Ideogram generate API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("resolution"));
        assert!(line.contains("seed"));
        assert!(line.contains("url"));
        assert!(body.contains("Image URL: https://img.test/a.png"));
    }

    #[test]
    fn absent_fields_not_claimed() {
        let out = format_summary("https://img.test/a.png", &[]);
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(footer.contains("Fields returned: url."));
        assert!(!footer.contains("seed"));
    }

    #[test]
    fn every_footer_field_in_body() {
        let fields = vec![Field {
            key: "seed",
            label: "Seed",
            value: "1".into(),
        }];
        let out = format_summary("https://img.test/a.png", &fields);
        let (body, footer) = out.split_once("---").unwrap();
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        let list = line
            .trim_start_matches("Fields returned:")
            .trim()
            .trim_end_matches('.');
        for field in list.split(", ") {
            let present = match field {
                "seed" => body.contains("Seed:"),
                "resolution" => body.contains("Resolution:"),
                "url" => body.contains("Image URL:"),
                other => panic!("unexpected footer field: {other}"),
            };
            assert!(present, "footer field '{field}' missing from body");
        }
    }
}
