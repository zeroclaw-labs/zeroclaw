//! ZeroClaw WASM plugin: computational knowledge via the Wolfram|Alpha LLM API.
//!
//! A stateless tool plugin — one request → one response, no stored state. Uses
//! Wolfram's LLM-optimized endpoint (`/api/v1/llm-api`), which returns a concise
//! plain-text answer designed for language models. Needs only the `http_client`
//! and `env_read` permissions.
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

const API_BASE: &str = "https://www.wolframalpha.com/api/v1/llm-api";
const API_KEY_ENV: &str = "WOLFRAM_APP_ID";
const MAX_RESULT_CHARS: usize = 8_000;

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

/// Build the model-facing output with the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(query: &str, result: &str, truncated: bool) -> String {
    let mut out = format!("Wolfram|Alpha — {query}\n\n{result}");
    if truncated {
        out.push_str(&format!(
            "\n\n[... truncated to {MAX_RESULT_CHARS} characters ...]"
        ));
    }
    out.push_str("\n\n---\n");
    out.push_str(
        "Data source: Wolfram|Alpha LLM API (https://www.wolframalpha.com/api/v1/llm-api).\n",
    );
    out.push_str("Fields returned: query, result.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "wolfram_query".into(),
        description: "Answer computational, mathematical, scientific, and factual questions using \
             Wolfram|Alpha (e.g. 'integral of x^2', 'distance from Earth to Mars', 'GDP of Japan \
             2023'). Returns a concise text answer."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The natural-language or mathematical query to evaluate."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Wolfram|Alpha query tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.trim().is_empty() => q.trim().to_string(),
        _ => return fail("Missing required parameter: 'query'"),
    };

    // ── Read API key via host function ────────────────────────────
    let app_id = match env_read(API_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => return fail(format!("App ID {API_KEY_ENV} is empty")),
        Err(e) => {
            return fail(format!(
                "Missing app id: set the {API_KEY_ENV} environment variable ({e})"
            ));
        }
    };

    // ── Call Wolfram via host HTTP function ───────────────────────
    let url = format!(
        "{API_BASE}?input={}&appid={}",
        percent_encode(&query),
        percent_encode(&app_id)
    );
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: std::collections::HashMap::new(),
        body: None,
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Wolfram request failed: {e}")),
    };
    if resp.status >= 400 {
        // Wolfram returns a plain-text explanation on error (e.g. 501 = no result).
        let body = resp.body.trim();
        return fail(format!(
            "Wolfram API error ({}): {}",
            resp.status,
            &body[..body.len().min(500)]
        ));
    }

    let result_raw = resp.body.trim();
    if result_raw.is_empty() {
        return fail("Wolfram returned an empty result for this query");
    }
    let truncated = result_raw.len() > MAX_RESULT_CHARS;
    let result = &result_raw[..result_raw.len().min(MAX_RESULT_CHARS)];

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&query, result, truncated),
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
        let out = format_summary("2+2", "4", false);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Wolfram|Alpha LLM API"));
        assert!(footer.contains("Fields returned: query, result."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Wolfram|Alpha — 2+2"));
        assert!(body.contains('4'));
    }

    #[test]
    fn truncation_disclosed() {
        let out = format_summary("q", "r", true);
        assert!(out.contains("truncated to"));
    }

    #[test]
    fn percent_encode_basics() {
        assert_eq!(percent_encode("a b"), "a%20b");
        assert_eq!(percent_encode("x^2"), "x%5E2");
        assert_eq!(percent_encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
    }
}
