//! ZeroClaw WASM plugin: relevance reranking for RAG via the Cohere Rerank API.
//!
//! A stateless tool plugin — one request → one response, no stored state. Takes a
//! query plus a list of candidate documents and returns them re-ordered by
//! semantic relevance (a two-stage-retrieval / RAG primitive). JSON in/out over
//! the standard (text) host HTTP bridge. Needs only the `http_client` and
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

const RERANK_URL: &str = "https://api.cohere.com/v2/rerank";
const API_KEY_ENV: &str = "COHERE_API_KEY";
const DEFAULT_MODEL: &str = "rerank-v3.5";
/// Cap each shown document so a large rerank can't flood the context window.
const MAX_DOC_CHARS: usize = 300;

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

/// Build the model-facing output (documents in relevance order) and the
/// mandatory fidelity footer (last, naming the source and listing exactly the
/// fields present).
fn format_summary(query: &str, ranked: &[(usize, f64, String)]) -> String {
    let mut out = format!("Reranked {} document(s) for: {query}\n", ranked.len());
    for (rank, (orig_index, score, doc)) in ranked.iter().enumerate() {
        let snippet = &doc[..doc.len().min(MAX_DOC_CHARS)];
        out.push_str(&format!(
            "{}. [score {score:.3}] (doc #{orig_index}) {snippet}\n",
            rank + 1
        ));
    }
    out.push_str("\n---\n");
    out.push_str("Data source: Cohere Rerank API (https://api.cohere.com/v2/rerank).\n");
    out.push_str("Fields returned: query, ranked_documents.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "rerank".into(),
        description:
            "Rerank a list of candidate documents by their semantic relevance to a query using \
             Cohere Rerank. Use after a keyword/vector search to put the most relevant results \
             first (two-stage retrieval / RAG). Returns documents in relevance order with scores."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["query", "documents"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to rank the documents against."
                },
                "documents": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The candidate document texts to rerank."
                },
                "top_n": {
                    "type": "integer",
                    "description": "Return only the top N most relevant documents (default: all)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Cohere rerank tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return fail("Missing required parameter: 'query'"),
    };
    let documents: Vec<String> = match args.get("documents").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect(),
        _ => return fail("Missing required parameter: 'documents' (a non-empty array of strings)"),
    };
    if documents.is_empty() {
        return fail("'documents' must contain at least one string");
    }
    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_MODEL);

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
    let mut body = serde_json::Map::new();
    body.insert("model".into(), json!(model));
    body.insert("query".into(), json!(query));
    body.insert("documents".into(), json!(documents));
    if let Some(top_n) = args.get("top_n").and_then(|v| v.as_u64()) {
        body.insert("top_n".into(), json!(top_n));
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: RERANK_URL.into(),
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
        Err(e) => return fail(format!("Cohere request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Cohere API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (results[] with index + relevance_score) ───
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Cohere response: {e}")))?;
    let results = match resp_json.get("results").and_then(|v| v.as_array()) {
        Some(r) if !r.is_empty() => r,
        _ => return fail("Cohere returned no rerank results"),
    };

    let ranked: Vec<(usize, f64, String)> = results
        .iter()
        .filter_map(|r| {
            let idx = r.get("index").and_then(|v| v.as_u64())? as usize;
            let score = r
                .get("relevance_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let doc = documents.get(idx).cloned().unwrap_or_default();
            Some((idx, score, doc))
        })
        .collect();

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&query, &ranked),
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
        let ranked = vec![
            (2usize, 0.95, "doc about rust".to_string()),
            (0usize, 0.40, "doc about python".to_string()),
        ];
        let out = format_summary("rust language", &ranked);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Cohere Rerank API"));
        assert!(footer.contains("Fields returned: query, ranked_documents."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Reranked 2 document(s) for: rust language"));
        assert!(body.contains("[score 0.950] (doc #2)"));
    }

    #[test]
    fn ranking_order_is_preserved() {
        let ranked = vec![
            (5usize, 0.9, "a".to_string()),
            (1usize, 0.1, "b".to_string()),
        ];
        let out = format_summary("q", &ranked);
        let first = out.find("(doc #5)").unwrap();
        let second = out.find("(doc #1)").unwrap();
        assert!(first < second, "highest-scored doc must come first");
    }
}
