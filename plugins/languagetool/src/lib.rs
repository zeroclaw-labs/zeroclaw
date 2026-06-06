//! ZeroClaw WASM plugin: grammar and style checking via LanguageTool.
//!
//! A stateless tool plugin — one request → one response, no stored state. Targets
//! a LanguageTool server (the public API by default, or the user's own
//! **self-hosted, open-source** instance via `LANGUAGETOOL_URL`). Form-encoded
//! request, JSON response over the standard (text) host HTTP bridge. Needs only
//! the `http_client` and `env_read` permissions.
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

/// Base URL of the LanguageTool server (public API by default; self-hostable).
const API_URL_ENV: &str = "LANGUAGETOOL_URL";
const DEFAULT_BASE: &str = "https://api.languagetool.org";
const MAX_MATCHES: usize = 50;

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

/// Percent-encode an application/x-www-form-urlencoded value.
fn form_encode(s: &str) -> String {
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

/// A grammar/style issue: message + the top suggested replacement (if any).
struct Issue {
    message: String,
    suggestion: String,
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(language: &str, issues: &[Issue]) -> String {
    let mut out = if issues.is_empty() {
        format!("No issues found (language: {language}).\n")
    } else {
        format!("Found {} issue(s) (language: {language}):\n", issues.len())
    };
    for (i, issue) in issues.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, issue.message));
        if !issue.suggestion.is_empty() {
            out.push_str(&format!("   suggestion: {}\n", issue.suggestion));
        }
    }
    out.push_str("\n---\n");
    out.push_str("Data source: LanguageTool /v2/check API.\n");
    out.push_str("Fields returned: language, matches.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "check_text".into(),
        description:
            "Check text for grammar, spelling, and style issues using LanguageTool, returning the \
             issues found and suggested corrections. Works with the public API or your own \
             self-hosted instance (set LANGUAGETOOL_URL)."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to check."
                },
                "language": {
                    "type": "string",
                    "description": "Language code (e.g. 'en-US', 'de-DE') or 'auto' to detect (default 'auto')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the LanguageTool check tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => return fail("Missing required parameter: 'text'"),
    };
    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("auto")
        .to_string();

    // ── Resolve the base URL (public by default; self-hostable) ───
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to a LanguageTool server"
        ));
    }

    // ── Call LanguageTool (form-encoded request) ──────────────────
    let body = format!(
        "text={}&language={}",
        form_encode(&text),
        form_encode(&language)
    );
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/v2/check"),
        headers: [(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        )]
        .into_iter()
        .collect(),
        body: Some(body),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("LanguageTool request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "LanguageTool error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response (matches[]) ────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse LanguageTool response: {e}")))?;
    let detected = resp_json
        .pointer("/language/code")
        .or_else(|| resp_json.pointer("/language/name"))
        .and_then(|v| v.as_str())
        .unwrap_or(&language)
        .to_string();
    let issues: Vec<Issue> = resp_json
        .get("matches")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .take(MAX_MATCHES)
                .map(|m| {
                    let message = m
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(issue)")
                        .to_string();
                    let suggestion = m
                        .pointer("/replacements/0/value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Issue {
                        message,
                        suggestion,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&detected, &issues),
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
        let issues = vec![Issue {
            message: "Possible spelling mistake".into(),
            suggestion: "the".into(),
        }];
        let out = format_summary("en-US", &issues);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: LanguageTool /v2/check API"));
        assert!(footer.contains("Fields returned: language, matches."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Found 1 issue(s) (language: en-US)"));
        assert!(body.contains("suggestion: the"));
    }

    #[test]
    fn no_issues_message() {
        let out = format_summary("en-US", &[]);
        assert!(out.contains("No issues found"));
    }

    #[test]
    fn form_encode_basics() {
        assert_eq!(form_encode("a b&c"), "a%20b%26c");
        assert_eq!(form_encode("Zz09-_.~"), "Zz09-_.~");
    }
}
