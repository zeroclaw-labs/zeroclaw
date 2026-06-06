//! ZeroClaw WASM plugin: read any URL as clean markdown via the Jina Reader API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! Jina Reader endpoint (`https://r.jina.ai/<url>`) is **keyless**; an optional
//! `JINA_API_KEY` raises rate limits when present. Uses host functions for the
//! HTTP request and (optional) key read, so it needs only the `http_client` and
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

const READER_PREFIX: &str = "https://r.jina.ai/";
/// Optional — raises rate limits when set. The plugin works keyless without it.
const API_KEY_ENV: &str = "JINA_API_KEY";
/// Cap the markdown returned to the model so one read can't flood the context.
const MAX_CONTENT_CHARS: usize = 12_000;

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

/// A metadata field actually returned by Jina Reader. The body and the fidelity
/// footer derive from the same set so the footer can't claim an absent field.
struct Field {
    key: &'static str,
    label: &'static str,
    value: String,
}

/// Build the model-facing output and the mandatory fidelity footer (last, naming
/// the source and the exact fields present — `content` is always included).
fn format_summary(requested_url: &str, fields: &[Field], content: &str, truncated: bool) -> String {
    let mut out = format!("Read {requested_url}\n\n");
    for f in fields {
        out.push_str(&format!("{}: {}\n", f.label, f.value));
    }
    out.push_str("\nContent (markdown):\n");
    out.push_str(content);
    if truncated {
        out.push_str(&format!(
            "\n\n[... truncated to {MAX_CONTENT_CHARS} characters ...]"
        ));
    }

    let mut keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
    keys.push("content");
    out.push_str("\n\n---\n");
    out.push_str("Data source: Jina Reader API (https://r.jina.ai).\n");
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "read_url".into(),
        description:
            "Read a single web page and return its content as clean, LLM-friendly markdown via the \
             Jina Reader service. Use this to fetch the readable contents of a specific URL."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The absolute URL of the page to read (must start with http:// or https://)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Jina Reader tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => return fail("Missing required parameter: 'url'"),
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return fail(format!(
            "Invalid url '{url}': must be an absolute http:// or https:// URL"
        ));
    }

    // ── Build headers (key is optional — works keyless) ───────────
    let mut headers: std::collections::HashMap<String, String> =
        [("Accept".to_string(), "application/json".to_string())]
            .into_iter()
            .collect();
    if let Ok(key) = env_read(API_KEY_ENV)
        && !key.trim().is_empty()
    {
        headers.insert("Authorization".into(), format!("Bearer {}", key.trim()));
    }

    // ── Call Jina Reader via host HTTP function ───────────────────
    let req = HttpRequest {
        method: "GET".into(),
        url: format!("{READER_PREFIX}{url}"),
        headers,
        body: None,
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Jina Reader request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Jina Reader error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ────────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Jina Reader response: {e}")))?;

    let content_raw = resp_json
        .pointer("/data/content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if content_raw.is_empty() {
        return fail("Jina Reader returned no content for this URL");
    }
    let truncated = content_raw.len() > MAX_CONTENT_CHARS;
    let content = &content_raw[..content_raw.len().min(MAX_CONTENT_CHARS)];

    let mut fields: Vec<Field> = Vec::new();
    if let Some(title) = resp_json.pointer("/data/title").and_then(|v| v.as_str())
        && !title.trim().is_empty()
    {
        fields.push(Field {
            key: "title",
            label: "Title",
            value: title.to_string(),
        });
    }
    if let Some(src) = resp_json.pointer("/data/url").and_then(|v| v.as_str()) {
        fields.push(Field {
            key: "source_url",
            label: "Source URL",
            value: src.to_string(),
        });
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&url, &fields, content, truncated),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_and_content_always_listed() {
        let fields = vec![
            Field {
                key: "title",
                label: "Title",
                value: "Example".into(),
            },
            Field {
                key: "source_url",
                label: "Source URL",
                value: "https://e.test".into(),
            },
        ];
        let out = format_summary("https://e.test", &fields, "# body", false);
        let (_body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: Jina Reader API"));
        assert!(out.trim_end().ends_with("not listed above."));
        let line = footer
            .lines()
            .find(|l| l.starts_with("Fields returned:"))
            .unwrap();
        assert!(line.contains("title"));
        assert!(line.contains("source_url"));
        assert!(line.contains("content"));
    }

    #[test]
    fn absent_metadata_not_claimed() {
        let out = format_summary("https://e.test", &[], "body", false);
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(footer.contains("Fields returned: content."));
        assert!(!footer.contains("title"));
    }

    #[test]
    fn truncation_disclosed() {
        let out = format_summary("https://e.test", &[], "body", true);
        assert!(out.contains("truncated to"));
    }

    #[test]
    fn every_footer_field_in_body() {
        let fields = vec![Field {
            key: "title",
            label: "Title",
            value: "T".into(),
        }];
        let out = format_summary("https://e.test", &fields, "c", false);
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
                "title" => body.contains("Title:"),
                "source_url" => body.contains("Source URL:"),
                "content" => body.contains("Content (markdown):"),
                other => panic!("unexpected footer field: {other}"),
            };
            assert!(present, "footer field '{field}' missing from body");
        }
    }
}
