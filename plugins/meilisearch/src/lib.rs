//! ZeroClaw WASM plugin: query a self-hosted Meilisearch index.
//!
//! A stateless tool plugin — one request → one response, no stored state. Runs a
//! full-text search against an index on the user's own **self-hosted, open-source**
//! Meilisearch server (configurable base URL), letting the agent search the user's
//! own data. JSON in/out over the standard (text) host HTTP bridge. Needs only the
//! `http_client` and `env_read` permissions.
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

/// Base URL of the self-hosted Meilisearch instance.
const API_URL_ENV: &str = "MEILISEARCH_URL";
const DEFAULT_BASE: &str = "http://localhost:7700";
/// Optional — a search/API key if the instance requires one.
const API_KEY_ENV: &str = "MEILISEARCH_API_KEY";
const DEFAULT_LIMIT: u64 = 5;
const MAX_LIMIT: u64 = 20;
/// Cap each hit's rendered JSON so a result set can't flood the context window.
const MAX_HIT_CHARS: usize = 600;

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

/// Build the model-facing output (each hit as compact JSON) and the mandatory
/// fidelity footer (last, naming the source and listing exactly the fields).
fn format_summary(index: &str, query: &str, hits: &[String]) -> String {
    let mut out = format!(
        "Meilisearch '{index}' — '{query}' ({} hit(s)):\n",
        hits.len()
    );
    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!("{}. {hit}\n", i + 1));
    }
    out.push_str("\n---\n");
    out.push_str("Data source: self-hosted Meilisearch (/indexes/<index>/search).\n");
    out.push_str("Fields returned: index, query, hits.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "search".into(),
        description:
            "Full-text search your own data in a self-hosted Meilisearch index. Provide the index \
             name and a query; returns the matching documents. Set MEILISEARCH_URL to your \
             instance."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["index", "query"],
            "properties": {
                "index": {
                    "type": "string",
                    "description": "The Meilisearch index (uid) to search."
                },
                "query": {
                    "type": "string",
                    "description": "The full-text search query."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of hits to return (1-20, default 5)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Meilisearch search tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let index = match args.get("index").and_then(|v| v.as_str()) {
        Some(i) if !i.trim().is_empty() => i.trim().to_string(),
        _ => return fail("Missing required parameter: 'index'"),
    };
    // The index goes straight into the request URL.
    if index.contains('/') || index.contains("..") || index.contains('?') || index.contains('#') {
        return fail(format!(
            "Invalid index '{index}': must be a Meilisearch index uid"
        ));
    }
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q.to_string(),
        None => return fail("Missing required parameter: 'query'"),
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);

    // ── Resolve the self-hosted base URL (defaults to localhost) ──
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your self-hosted Meilisearch"
        ));
    }

    // ── Build headers (optional API key) ──────────────────────────
    let mut headers: std::collections::HashMap<String, String> =
        [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect();
    if let Ok(key) = env_read(API_KEY_ENV)
        && !key.trim().is_empty()
    {
        headers.insert("Authorization".into(), format!("Bearer {}", key.trim()));
    }

    // ── Call Meilisearch ──────────────────────────────────────────
    let body = json!({ "q": query, "limit": limit });
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/indexes/{index}/search"),
        headers,
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "Meilisearch request failed: {e}. Is your instance running at {base}?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "Meilisearch error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (hits[]) ───────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Meilisearch response: {e}")))?;
    let hits: Vec<String> = resp_json
        .get("hits")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|h| {
                    let s = h.to_string();
                    s[..s.len().min(MAX_HIT_CHARS)].to_string()
                })
                .collect()
        })
        .unwrap_or_default();

    if hits.is_empty() {
        return fail(format!(
            "No hits in Meilisearch index '{index}' for '{query}'"
        ));
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&index, &query, &hits),
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
        let hits = vec!["{\"title\":\"Doc A\"}".to_string()];
        let out = format_summary("docs", "a", &hits);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: self-hosted Meilisearch"));
        assert!(footer.contains("Fields returned: index, query, hits."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Meilisearch 'docs' — 'a' (1 hit(s))"));
        assert!(body.contains("Doc A"));
    }

    #[test]
    fn reports_hit_count() {
        let hits = vec!["{}".to_string(), "{}".to_string()];
        let out = format_summary("i", "q", &hits);
        assert!(out.contains("(2 hit(s))"));
    }
}
