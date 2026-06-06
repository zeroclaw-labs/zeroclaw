//! ZeroClaw WASM plugin: agent-grade web search via the Tavily API.
//!
//! A stateless tool plugin — one request → one response, no stored state. Uses
//! host functions for the outbound HTTP request and to read the API key, so it
//! needs only the `http_client` and `env_read` permissions.
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

const SEARCH_URL: &str = "https://api.tavily.com/search";
const API_KEY_ENV: &str = "TAVILY_API_KEY";
const DEFAULT_MAX_RESULTS: u64 = 5;
const MAX_RESULTS_CAP: u64 = 20;
/// Cap each result's snippet so a multi-result search can't flood the context.
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

// ── Output formatting ─────────────────────────────────────────────

/// Build the model-facing output and the mandatory fidelity footer. The footer
/// lists exactly the fields present (`query` always, `answer` only when Tavily
/// returned one, `results` only when non-empty), so it can never claim a field
/// the body lacks.
fn format_summary(
    query: &str,
    answer: Option<&str>,
    results: &[(String, String, String)],
) -> String {
    let mut out = format!("Web search: {query}\n");

    let mut keys: Vec<&str> = vec!["query"];

    if let Some(a) = answer {
        out.push_str(&format!("\nAnswer: {a}\n"));
        keys.push("answer");
    }

    if !results.is_empty() {
        keys.push("results");
        out.push_str(&format!("\nResults ({}):\n", results.len()));
        for (i, (title, url, snippet)) in results.iter().enumerate() {
            out.push_str(&format!("{}. {title}\n   {url}\n   {snippet}\n", i + 1));
        }
    }

    out.push_str("\n---\n");
    out.push_str("Data source: Tavily search API (https://api.tavily.com/search).\n");
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
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
            "Search the web and return ranked results (title, URL, content snippet) plus an \
             optional direct answer, via the Tavily search API. Use this to find current \
             information or sources for a query."
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
                },
                "search_depth": {
                    "type": "string",
                    "enum": ["basic", "advanced"],
                    "description": "Search depth: 'basic' (fast) or 'advanced' (deeper). Default 'basic'."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Tavily web search tool.
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
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, MAX_RESULTS_CAP);
    let search_depth = match args.get("search_depth").and_then(|v| v.as_str()) {
        Some("advanced") => "advanced",
        Some("basic") | None => "basic",
        Some(other) => {
            return fail(format!(
                "Invalid search_depth '{other}': use 'basic' or 'advanced'"
            ));
        }
    };

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

    // ── Call Tavily via host HTTP function ────────────────────────
    let body = json!({
        "query": query,
        "max_results": max_results,
        "search_depth": search_depth,
        "include_answer": true
    });
    let req = HttpRequest {
        method: "POST".into(),
        url: SEARCH_URL.into(),
        headers: [
            ("Authorization".into(), format!("Bearer {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Tavily request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Tavily API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ────────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Tavily response: {e}")))?;

    let answer = resp_json
        .get("answer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());

    let results: Vec<(String, String, String)> = resp_json
        .get("results")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
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

    if answer.is_none() && results.is_empty() {
        return fail("Tavily returned no answer and no results for this query");
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&query, answer, &results),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn results() -> Vec<(String, String, String)> {
        vec![("T".into(), "https://e.test".into(), "snippet".into())]
    }

    #[test]
    fn footer_present_last_and_lists_query() {
        let out = format_summary("rust", Some("Rust is a language"), &results());
        let (_body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Tavily search API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("query"));
        assert!(line.contains("answer"));
        assert!(line.contains("results"));
    }

    #[test]
    fn answer_omitted_when_absent() {
        let out = format_summary("rust", None, &results());
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("answer"));
        assert!(footer.contains("results"));
    }

    #[test]
    fn results_omitted_when_empty() {
        let out = format_summary("rust", Some("a"), &[]);
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("results"));
        assert!(footer.contains("answer"));
    }

    #[test]
    fn every_footer_field_appears_in_body() {
        let out = format_summary("rust", Some("ans"), &results());
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
                "query" => body.contains("Web search:"),
                "answer" => body.contains("Answer:"),
                "results" => body.contains("Results ("),
                other => panic!("unexpected footer field: {other}"),
            };
            assert!(present, "footer field '{field}' missing from body");
        }
    }
}
