//! ZeroClaw WASM plugin: private web search via a self-hosted SearXNG instance.
//!
//! A stateless tool plugin — one request → one response, no stored state. Targets
//! the user's own **self-hosted, keyless** SearXNG meta-search engine
//! (configurable base URL), so search queries stay on the user's infrastructure —
//! no third-party search API, no key, no lock-in. JSON response over the standard
//! (text) host HTTP bridge. Needs only the `http_client` and `env_read`
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

/// Base URL of the self-hosted SearXNG instance.
const API_URL_ENV: &str = "SEARXNG_URL";
const DEFAULT_BASE: &str = "http://localhost:8080";
const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_CAP: usize = 20;
const MAX_SNIPPET_CHARS: usize = 800;

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

/// Percent-encode a query-string value (RFC 3986 unreserved set kept as-is).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(query: &str, results: &[(String, String, String)]) -> String {
    let mut out = format!("Search results for: {query} ({})\n", results.len());
    for (i, (title, url, snippet)) in results.iter().enumerate() {
        out.push_str(&format!("{}. {title}\n   {url}\n   {snippet}\n", i + 1));
    }
    out.push_str("\n---\n");
    out.push_str("Data source: self-hosted SearXNG instance (/search?format=json).\n");
    out.push_str("Fields returned: query, results.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "web_search".into(),
        description:
            "Search the web privately through your own self-hosted SearXNG meta-search engine. \
             Returns ranked results (title, URL, snippet). Set SEARXNG_URL to your instance."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (1-20, default 5)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the SearXNG search tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return fail("Missing required parameter: 'query'"),
    };
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).clamp(1, MAX_RESULTS_CAP))
        .unwrap_or(DEFAULT_MAX_RESULTS);

    // ── Resolve the self-hosted base URL (defaults to localhost) ──
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your self-hosted SearXNG"
        ));
    }

    // ── Call SearXNG via host HTTP function ───────────────────────
    let url = format!("{base}/search?q={}&format=json", percent_encode(&query));
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: std::collections::HashMap::new(),
        body: None,
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "SearXNG request failed: {e}. Is your instance running at {base} with the JSON format enabled?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "SearXNG error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (results[]) ────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse SearXNG response: {e}")))?;
    let results: Vec<(String, String, String)> = resp_json
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(max_results)
                .filter_map(|r| {
                    let title = r
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(untitled)");
                    let url = r.get("url").and_then(|v| v.as_str())?;
                    let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let snippet = &content[..content.len().min(MAX_SNIPPET_CHARS)];
                    Some((title.to_string(), url.to_string(), snippet.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    if results.is_empty() {
        return fail(format!("No results from SearXNG for '{query}'"));
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&query, &results),
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
        let results = vec![("T".into(), "https://e.test".into(), "snippet".into())];
        let out = format_summary("rust", &results);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: self-hosted SearXNG"));
        assert!(footer.contains("Fields returned: query, results."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Search results for: rust"));
    }

    #[test]
    fn percent_encode_basics() {
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
        assert_eq!(percent_encode("Zz09-_.~"), "Zz09-_.~");
    }
}
