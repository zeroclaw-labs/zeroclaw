//! ZeroClaw WASM plugin: web-grounded answers with citations via the Perplexity API.
//!
//! A stateless tool plugin — one request → one response, no stored state. Returns
//! a synthesized answer plus its source citations (an answer engine, not a list
//! of links). OpenAI-compatible JSON in/out over the standard (text) host HTTP
//! bridge. Needs only the `http_client` and `env_read` permissions.
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

const API_URL: &str = "https://api.perplexity.ai/chat/completions";
const API_KEY_ENV: &str = "PERPLEXITY_API_KEY";
const DEFAULT_MODEL: &str = "sonar";

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

/// Collect citation URLs from either the `citations` (array of url strings) or
/// `search_results` (array of `{url, title}`) field, whichever the API returned.
fn collect_citations(resp: &serde_json::Value) -> Vec<String> {
    if let Some(arr) = resp.get("citations").and_then(|v| v.as_array()) {
        let urls: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect();
        if !urls.is_empty() {
            return urls;
        }
    }
    if let Some(arr) = resp.get("search_results").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|r| r.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect();
    }
    Vec::new()
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present — `citations` only
/// when the API returned any).
fn format_summary(query: &str, answer: &str, citations: &[String]) -> String {
    let mut out = format!("Answer ({query}):\n\n{answer}\n");
    let mut keys: Vec<&str> = vec!["query", "answer"];
    if !citations.is_empty() {
        keys.push("citations");
        out.push_str("\nSources:\n");
        for (i, c) in citations.iter().enumerate() {
            out.push_str(&format!("  [{}] {c}\n", i + 1));
        }
    }
    out.push_str("\n---\n");
    out.push_str("Data source: Perplexity API (https://api.perplexity.ai/chat/completions).\n");
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "ask".into(),
        description:
            "Ask a question and get a concise, web-grounded answer with source citations using \
             Perplexity (an answer engine). Use when you need a synthesized answer with sources \
             rather than a list of links."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The question to answer."
                },
                "model": {
                    "type": "string",
                    "description": "Perplexity model (default 'sonar'; e.g. 'sonar-pro', 'sonar-reasoning')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Perplexity ask tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return fail("Missing required parameter: 'query'"),
    };
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

    // ── Build the JSON body (OpenAI chat-completions shape) ───────
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": query }]
    });
    let req = HttpRequest {
        method: "POST".into(),
        url: API_URL.into(),
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
        Err(e) => return fail(format!("Perplexity request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Perplexity API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (choices[0].message.content + citations) ───
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Perplexity response: {e}")))?;
    let answer = match resp_json
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
    {
        Some(a) if !a.trim().is_empty() => a.trim(),
        _ => return fail("Perplexity returned no answer content"),
    };
    let citations = collect_citations(&resp_json);

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&query, answer, &citations),
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
        let out = format_summary("who?", "the answer", &["https://a.test".to_string()]);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Perplexity API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("query"));
        assert!(line.contains("answer"));
        assert!(line.contains("citations"));
        assert!(body.contains("the answer"));
        assert!(body.contains("[1] https://a.test"));
    }

    #[test]
    fn citations_omitted_when_absent() {
        let out = format_summary("q", "a", &[]);
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(!footer.contains("citations"));
        assert!(footer.contains("answer"));
    }

    #[test]
    fn collect_citations_handles_both_shapes() {
        let a = json!({ "citations": ["https://x.test", "https://y.test"] });
        assert_eq!(collect_citations(&a).len(), 2);
        let b = json!({ "search_results": [{ "url": "https://z.test", "title": "Z" }] });
        assert_eq!(collect_citations(&b), vec!["https://z.test"]);
        let c = json!({});
        assert!(collect_citations(&c).is_empty());
    }
}
