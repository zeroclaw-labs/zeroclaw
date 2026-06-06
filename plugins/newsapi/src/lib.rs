//! ZeroClaw WASM plugin: search recent news articles via NewsAPI.org.
//!
//! A stateless tool plugin — one request → one response, no stored state. JSON
//! response over the standard (text) host HTTP bridge. Needs only the
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

const API_BASE: &str = "https://newsapi.org/v2/everything";
const API_KEY_ENV: &str = "NEWSAPI_KEY";
const DEFAULT_PAGE_SIZE: u64 = 5;
const MAX_PAGE_SIZE: u64 = 20;

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
fn format_summary(query: &str, articles: &[(String, String, String, String)]) -> String {
    let mut out = format!("News results for: {query} ({})\n", articles.len());
    for (i, (title, source, date, url)) in articles.iter().enumerate() {
        out.push_str(&format!(
            "{}. {title}\n   {source} · {date}\n   {url}\n",
            i + 1
        ));
    }
    out.push_str("\n---\n");
    out.push_str(
        "Data source: NewsAPI.org everything endpoint (https://newsapi.org/v2/everything).\n",
    );
    out.push_str("Fields returned: query, articles.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "news_search".into(),
        description:
            "Search recent news articles by keyword using NewsAPI.org. Returns a ranked list of \
             articles (title, source, date, URL), most recent first."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or phrase to search for in news articles."
                },
                "page_size": {
                    "type": "integer",
                    "description": "Number of articles to return (1-20, default 5)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the NewsAPI search tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return fail("Missing required parameter: 'query'"),
    };
    let page_size = args
        .get("page_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);

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

    // ── Call NewsAPI via host HTTP function ───────────────────────
    let url = format!(
        "{API_BASE}?q={}&pageSize={page_size}&sortBy=publishedAt&language=en",
        percent_encode(&query)
    );
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: [("X-Api-Key".into(), api_key)].into_iter().collect(),
        body: None,
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("NewsAPI request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "NewsAPI error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (articles[]) ───────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse NewsAPI response: {e}")))?;
    let articles: Vec<(String, String, String, String)> = resp_json
        .get("articles")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let title = a.get("title").and_then(|v| v.as_str())?;
                    let url = a.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let source = a
                        .pointer("/source/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let date = a.get("publishedAt").and_then(|v| v.as_str()).unwrap_or("");
                    Some((
                        title.to_string(),
                        source.to_string(),
                        date.to_string(),
                        url.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    if articles.is_empty() {
        return fail(format!("No news articles found for '{query}'"));
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&query, &articles),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(String, String, String, String)> {
        vec![(
            "Headline".into(),
            "Reuters".into(),
            "2026-06-06".into(),
            "https://n.test/a".into(),
        )]
    }

    #[test]
    fn footer_present_last_lists_fields() {
        let out = format_summary("rust", &sample());
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: NewsAPI.org"));
        assert!(footer.contains("Fields returned: query, articles."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("News results for: rust"));
        assert!(body.contains("Headline"));
        assert!(body.contains("Reuters · 2026-06-06"));
    }

    #[test]
    fn percent_encode_basics() {
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
        assert_eq!(percent_encode("Zz09-_.~"), "Zz09-_.~");
    }
}
