//! ZeroClaw WASM plugin: local text embeddings via a self-hosted Ollama instance.
//!
//! A stateless tool plugin — one request → one response, no stored state. Targets
//! the user's own **local, keyless** Ollama server (configurable base URL), so
//! embeddings are computed on the user's own hardware — fully private, no
//! third-party API. JSON in/out over the standard (text) host HTTP bridge. Needs
//! only the `http_client` and `env_read` permissions.
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

/// Base URL of the local Ollama instance.
const API_URL_ENV: &str = "OLLAMA_URL";
const DEFAULT_BASE: &str = "http://localhost:11434";
const DEFAULT_MODEL: &str = "nomic-embed-text";
const MAX_INPUTS: usize = 128;

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

/// Build the model-facing output (header + vectors as JSON for programmatic
/// use) and the mandatory fidelity footer (last, naming the source and listing
/// exactly the fields present).
fn format_summary(model: &str, dims: usize, embeddings: &[Vec<f64>]) -> String {
    let mut out = format!(
        "Embeddings ({model}, local Ollama)\nInputs: {}\nDimensions: {dims}\n\nVectors (JSON, for programmatic use):\n",
        embeddings.len()
    );
    out.push_str(&serde_json::to_string(embeddings).unwrap_or_else(|_| "[]".into()));

    out.push_str("\n\n---\n");
    out.push_str("Data source: local Ollama embeddings (/api/embed).\n");
    out.push_str("Fields returned: model, dimensions, embeddings.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "embed".into(),
        description:
            "Generate text embedding vectors locally on your own Ollama instance (fully private, \
             no third-party API) for semantic search / RAG. Returns the vectors as JSON. Set \
             OLLAMA_URL to your instance and a local embedding model."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["input"],
            "properties": {
                "input": {
                    "description": "A text string, or an array of text strings, to embed."
                },
                "model": {
                    "type": "string",
                    "description": "Local Ollama embedding model (default 'nomic-embed-text'; e.g. 'mxbai-embed-large')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Ollama embeddings tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters (input: string or array) ──────
    let inputs: Vec<String> = match args.get("input") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => vec![s.clone()],
        Some(serde_json::Value::Array(arr)) if !arr.is_empty() => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect(),
        _ => return fail("Missing required parameter: 'input' (a string or array of strings)"),
    };
    if inputs.is_empty() {
        return fail("'input' must contain at least one non-empty string");
    }
    if inputs.len() > MAX_INPUTS {
        return fail(format!(
            "Too many inputs ({}); maximum is {MAX_INPUTS}",
            inputs.len()
        ));
    }
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_MODEL);

    // ── Resolve the local base URL (defaults to localhost) ────────
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your Ollama instance"
        ));
    }

    // ── Call Ollama via host HTTP function ────────────────────────
    let body = json!({ "model": model, "input": inputs });
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/api/embed"),
        headers: [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "Ollama request failed: {e}. Is Ollama running at {base} with the '{model}' model pulled?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "Ollama API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (embeddings: [[..]]) ───────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Ollama response: {e}")))?;
    let data = match resp_json.get("embeddings").and_then(|v| v.as_array()) {
        Some(d) if !d.is_empty() => d,
        _ => return fail("Ollama returned no embeddings (is the model an embedding model?)"),
    };
    let embeddings: Vec<Vec<f64>> = data
        .iter()
        .filter_map(|e| {
            e.as_array()
                .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
        })
        .collect();
    if embeddings.is_empty() {
        return fail("Ollama response contained no embedding vectors");
    }
    let dims = embeddings[0].len();

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(model, dims, &embeddings),
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
        let emb = vec![vec![0.1, 0.2, 0.3]];
        let out = format_summary("nomic-embed-text", 3, &emb);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: local Ollama embeddings"));
        assert!(footer.contains("Fields returned: model, dimensions, embeddings."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("local Ollama"));
        assert!(body.contains("Dimensions: 3"));
        assert!(body.contains("[[0.1,0.2,0.3]]"));
    }

    #[test]
    fn reports_input_count() {
        let emb = vec![vec![0.0], vec![1.0]];
        let out = format_summary("m", 1, &emb);
        assert!(out.contains("Inputs: 2"));
    }
}
