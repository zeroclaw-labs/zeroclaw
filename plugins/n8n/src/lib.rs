//! ZeroClaw WASM plugin: trigger workflows on a self-hosted n8n instance.
//!
//! A stateless tool plugin — one request → one response, no stored state. POSTs a
//! payload to a workflow's webhook on the user's own **self-hosted, open-source**
//! n8n automation server (configurable base URL), and returns the workflow's
//! response. Because n8n connects to hundreds of services, this single plugin
//! lets the agent reach the user's entire n8n integration catalog. JSON over the
//! standard (text) host HTTP bridge. Needs only the `http_client` and `env_read`
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

/// Base URL of the self-hosted n8n instance.
const API_URL_ENV: &str = "N8N_BASE_URL";
const DEFAULT_BASE: &str = "http://localhost:5678";
/// Optional — sent as a Bearer token if the webhook is protected.
const API_TOKEN_ENV: &str = "N8N_AUTH_TOKEN";
/// Cap the workflow response shown to the model.
const MAX_RESPONSE_CHARS: usize = 8_000;

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

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
/// Slicing on a raw byte index (e.g. an error body or a long workflow response)
/// can land inside a multi-byte character and panic; this walks back to the
/// nearest char boundary at or before `max`.
fn truncate_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── Output formatting ─────────────────────────────────────────────

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(webhook_path: &str, response: &str, truncated: bool) -> String {
    let mut out =
        format!("Triggered n8n workflow: {webhook_path}\n\nWorkflow response:\n{response}");
    if truncated {
        out.push_str(&format!(
            "\n\n[... truncated to {MAX_RESPONSE_CHARS} characters ...]"
        ));
    }
    out.push_str("\n\n---\n");
    out.push_str("Data source: self-hosted n8n webhook (/webhook/<path>).\n");
    out.push_str("Fields returned: webhook_path, response.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "trigger_workflow".into(),
        description:
            "Trigger an automation workflow on your self-hosted n8n instance by its webhook path, \
             passing an optional JSON payload, and return the workflow's response. Use this to run \
             any automation you've built in n8n (which can connect to hundreds of apps). Set \
             N8N_BASE_URL to your instance."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["webhook_path"],
            "properties": {
                "webhook_path": {
                    "type": "string",
                    "description": "The workflow's webhook path (the part after '/webhook/')."
                },
                "data": {
                    "type": "object",
                    "description": "Optional JSON payload to send to the workflow."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the n8n trigger tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let webhook_path = match args.get("webhook_path").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().trim_start_matches('/').to_string(),
        _ => return fail("Missing required parameter: 'webhook_path'"),
    };
    // The path goes straight into the request URL.
    if webhook_path.contains("..") || webhook_path.contains('?') || webhook_path.contains('#') {
        return fail(format!("Invalid webhook_path '{webhook_path}'"));
    }
    let data = args.get("data").cloned().unwrap_or_else(|| json!({}));

    // ── Resolve the self-hosted base URL (defaults to localhost) ──
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your self-hosted n8n"
        ));
    }

    // ── Build headers (optional bearer token) ─────────────────────
    let mut headers: std::collections::HashMap<String, String> =
        [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect();
    if let Ok(tok) = env_read(API_TOKEN_ENV)
        && !tok.trim().is_empty()
    {
        headers.insert("Authorization".into(), format!("Bearer {}", tok.trim()));
    }

    // ── Trigger the workflow webhook ──────────────────────────────
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/webhook/{webhook_path}"),
        headers,
        body: Some(serde_json::to_string(&data)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "n8n request failed: {e}. Is your instance running at {base} and the workflow active?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "n8n webhook error ({}): {}",
            resp.status,
            truncate_chars(&resp.body, 500)
        ));
    }

    let response_raw = resp.body.trim();
    let truncated = response_raw.len() > MAX_RESPONSE_CHARS;
    let response = truncate_chars(response_raw, MAX_RESPONSE_CHARS);

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(
            &webhook_path,
            if response.is_empty() {
                "(empty)"
            } else {
                response
            },
            truncated,
        ),
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
        let out = format_summary("my-flow", "{\"ok\":true}", false);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: self-hosted n8n webhook"));
        assert!(footer.contains("Fields returned: webhook_path, response."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Triggered n8n workflow: my-flow"));
        assert!(body.contains("{\"ok\":true}"));
    }

    #[test]
    fn truncation_disclosed() {
        let out = format_summary("f", "x", true);
        assert!(out.contains("truncated to"));
    }

    #[test]
    fn truncate_chars_never_splits_multibyte_error_and_response() {
        // Both the error preview (cap 500) and the success response (cap
        // MAX_RESPONSE_CHARS) slice the body by index; a cutoff landing inside a
        // multi-byte character must not panic and must stay on a char boundary.
        let err_body = "é".repeat(400); // 800 bytes; 500 cutoff is mid-char
        let cut = truncate_chars(&err_body, 500);
        assert!(cut.len() <= 500);
        assert!(err_body.is_char_boundary(cut.len()));

        // Long non-ASCII successful workflow response.
        let resp_body = "ü".repeat(MAX_RESPONSE_CHARS); // 2 bytes each
        let resp_cut = truncate_chars(&resp_body, MAX_RESPONSE_CHARS);
        assert!(resp_cut.len() <= MAX_RESPONSE_CHARS);
        assert!(resp_body.is_char_boundary(resp_cut.len()));
        // Formats without panicking.
        let _ = format_summary("f", resp_cut, true);
    }

    #[test]
    fn truncate_chars_short_input_unchanged() {
        assert_eq!(truncate_chars("hello", 500), "hello");
        assert_eq!(truncate_chars("", 500), "");
    }
}
