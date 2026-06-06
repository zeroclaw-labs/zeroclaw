//! ZeroClaw WASM plugin: run text/JSON Hugging Face Inference API models.
//!
//! A stateless tool plugin — one request → one response, no stored state. Uses
//! host functions for the outbound HTTP request and to read the API token, so it
//! needs only the `http_client` and `env_read` permissions.
//!
//! Scope: **text/JSON tasks** (text-generation, summarization, translation,
//! classification, feature-extraction/embeddings). Models that return binary
//! (image/audio) are NOT supported — the host's HTTP bridge is text-only — and
//! are rejected with a clear error rather than mangled.
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

const API_BASE: &str = "https://api-inference.huggingface.co/models/";
const API_KEY_ENV: &str = "HF_TOKEN";
/// Cap the rendered output so a large generation can't flood the context window.
const MAX_OUTPUT_CHARS: usize = 12_000;

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

// ── Output rendering ──────────────────────────────────────────────

/// Render Hugging Face's task-dependent JSON output into a single display
/// string, recognising the common text/JSON task shapes and falling back to
/// compact JSON for anything else.
fn render_output(v: &serde_json::Value) -> String {
    // Most pipelines return a single-element array.
    if let Some(arr) = v.as_array()
        && let Some(first) = arr.first()
    {
        // text-generation / summarization / translation
        for key in ["generated_text", "summary_text", "translation_text"] {
            if let Some(s) = first.get(key).and_then(|x| x.as_str()) {
                return s.to_string();
            }
        }
        // text-classification: [{label, score}, ...]
        if first.get("label").is_some() {
            let labels: Vec<String> = arr
                .iter()
                .filter_map(|e| {
                    let label = e.get("label").and_then(|x| x.as_str())?;
                    let score = e.get("score").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    Some(format!("{label} ({score:.3})"))
                })
                .collect();
            if !labels.is_empty() {
                return labels.join(", ");
            }
        }
        // feature-extraction / embeddings: nested float arrays — summarize.
        if first.is_array() || first.is_number() {
            return format!("embedding/array output ({} elements)", arr.len());
        }
    }
    // Single object with a generated_text field.
    if let Some(s) = v.get("generated_text").and_then(|x| x.as_str()) {
        return s.to_string();
    }
    // Fallback: compact JSON.
    v.to_string()
}

/// Build the model-facing output and the mandatory fidelity footer (last, naming
/// the source and the exact fields present).
fn format_summary(model: &str, output: &str, truncated: bool) -> String {
    let mut out = format!("Hugging Face model: {model}\n\nOutput:\n{output}");
    if truncated {
        out.push_str(&format!(
            "\n\n[... truncated to {MAX_OUTPUT_CHARS} characters ...]"
        ));
    }
    out.push_str("\n\n---\n");
    out.push_str(
        "Data source: Hugging Face Inference API (https://api-inference.huggingface.co).\n",
    );
    out.push_str("Fields returned: model, output.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "hf_infer".into(),
        description:
            "Run a text/JSON model on the Hugging Face Inference API and return its output. \
             Supports text generation, summarization, translation, classification, and embeddings. \
             Requires the model id (e.g. 'mistralai/Mistral-7B-Instruct-v0.2') and the model's \
             'inputs'. Does NOT support image/audio (binary) models."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["model", "inputs"],
            "properties": {
                "model": {
                    "type": "string",
                    "description": "The Hugging Face model id, e.g. 'facebook/bart-large-cnn'."
                },
                "inputs": {
                    "description": "The model inputs — usually a string (prompt/text), or a model-specific object."
                },
                "parameters": {
                    "type": "object",
                    "description": "Optional task parameters (e.g. {\"max_new_tokens\": 256})."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Hugging Face inference tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let model = match args.get("model").and_then(|v| v.as_str()) {
        Some(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => return fail("Missing required parameter: 'model'"),
    };
    // Guard the model path: it goes straight into the request URL.
    if model.contains("..") || model.starts_with('/') || model.contains('?') || model.contains('#')
    {
        return fail(format!(
            "Invalid model id '{model}': must be a Hugging Face model path like 'owner/name'"
        ));
    }
    let inputs = match args.get("inputs") {
        Some(v) if !v.is_null() => v.clone(),
        _ => return fail("Missing required parameter: 'inputs'"),
    };

    // ── Read API token via host function ──────────────────────────
    let api_key = match env_read(API_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => return fail(format!("API token {API_KEY_ENV} is empty")),
        Err(e) => {
            return fail(format!(
                "Missing API token: set the {API_KEY_ENV} environment variable ({e})"
            ));
        }
    };

    // ── Build request body ────────────────────────────────────────
    let mut body = json!({ "inputs": inputs });
    if let Some(params) = args.get("parameters")
        && params.is_object()
    {
        body["parameters"] = params.clone();
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{API_BASE}{model}"),
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
        Err(e) => return fail(format!("Hugging Face request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Hugging Face API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (JSON only — binary models are out of scope) ─
    let resp_json: serde_json::Value = match serde_json::from_str(&resp.body) {
        Ok(j) => j,
        Err(_) => {
            return fail(
                "Hugging Face returned a non-JSON response. This model likely produces binary \
                 output (image/audio), which this plugin does not support — use a text/JSON model.",
            );
        }
    };

    let rendered = render_output(&resp_json);
    let truncated = rendered.len() > MAX_OUTPUT_CHARS;
    let output = &rendered[..rendered.len().min(MAX_OUTPUT_CHARS)];

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&model, output, truncated),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_lists_model_and_output() {
        let out = format_summary("owner/name", "hello", false);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Hugging Face Inference API"));
        assert!(footer.contains("Fields returned: model, output."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Hugging Face model: owner/name"));
        assert!(body.contains("Output:"));
    }

    #[test]
    fn render_text_generation() {
        let v = json!([{ "generated_text": "the answer" }]);
        assert_eq!(render_output(&v), "the answer");
    }

    #[test]
    fn render_classification() {
        let v =
            json!([{ "label": "POSITIVE", "score": 0.99 }, { "label": "NEGATIVE", "score": 0.01 }]);
        let r = render_output(&v);
        assert!(r.contains("POSITIVE"));
        assert!(r.contains("NEGATIVE"));
    }

    #[test]
    fn render_embeddings_summarized() {
        let v = json!([0.1, 0.2, 0.3]);
        assert!(render_output(&v).contains("3 elements"));
    }

    #[test]
    fn truncation_disclosed() {
        let out = format_summary("m", "x", true);
        assert!(out.contains("truncated to"));
    }
}
