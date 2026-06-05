//! ZeroClaw WASM plugin: scrape any URL to clean markdown via the Firecrawl API.
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

const SCRAPE_URL: &str = "https://api.firecrawl.dev/v2/scrape";
const API_KEY_ENV: &str = "FIRECRAWL_API_KEY";
/// Cap the markdown returned to the model so a single scrape can't flood the
/// context window. Firecrawl pages can be very large.
const MAX_MARKDOWN_CHARS: usize = 12_000;

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

/// A single field actually extracted from the Firecrawl response. Both the body
/// and the fidelity footer are derived from the same set, so the footer can
/// never claim a field the body doesn't contain.
struct Field {
    /// snake_case name listed in the fidelity footer.
    key: &'static str,
    /// Human label shown in the body.
    label: &'static str,
    value: String,
}

/// Build the model-facing output: a header, the extracted metadata fields, the
/// markdown content block, and the mandatory fidelity footer naming the data
/// source and listing exactly the fields present. Per the output-fidelity rule,
/// the footer is last and lists only fields that actually appear above it.
fn format_summary(
    requested_url: &str,
    fields: &[Field],
    markdown: &str,
    truncated: bool,
) -> String {
    let mut out = format!("Scraped {requested_url}\n\n");

    for f in fields {
        out.push_str(&format!("{}: {}\n", f.label, f.value));
    }

    out.push_str("\nContent (markdown):\n");
    out.push_str(markdown);
    if truncated {
        out.push_str(&format!(
            "\n\n[... truncated to {MAX_MARKDOWN_CHARS} characters ...]"
        ));
    }

    // Fidelity footer — names the source and the exact fields returned, so the
    // model cannot fabricate fields Firecrawl never sent.
    let mut keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
    keys.push("markdown");
    out.push_str("\n\n---\n");
    out.push_str("Data source: Firecrawl scrape API (https://api.firecrawl.dev/v2/scrape).\n");
    out.push_str(&format!("Fields returned: {}.\n", keys.join(", ")));
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "firecrawl_scrape".into(),
        description:
            "Scrape a single web page and return its main content as clean markdown using the \
             Firecrawl API. Use this to read the contents of a specific URL."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The absolute URL of the page to scrape (must start with http:// or https://)."
                },
                "only_main_content": {
                    "type": "boolean",
                    "description": "Strip navigation, headers, and footers, returning only the main content (default: true)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Firecrawl scrape tool.
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
    let only_main_content = args
        .get("only_main_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

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

    // ── Call Firecrawl via host HTTP function ─────────────────────
    let body = json!({
        "url": url,
        "formats": ["markdown"],
        "onlyMainContent": only_main_content
    });
    let req = HttpRequest {
        method: "POST".into(),
        url: SCRAPE_URL.into(),
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
        Err(e) => return fail(format!("Firecrawl request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Firecrawl API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ────────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Firecrawl response: {e}")))?;

    let markdown_raw = resp_json
        .pointer("/data/markdown")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if markdown_raw.is_empty() {
        return fail("Firecrawl returned no markdown content for this URL");
    }
    let truncated = markdown_raw.len() > MAX_MARKDOWN_CHARS;
    let markdown = &markdown_raw[..markdown_raw.len().min(MAX_MARKDOWN_CHARS)];

    // Only include metadata fields that Firecrawl actually returned.
    let mut fields: Vec<Field> = Vec::new();
    if let Some(title) = resp_json.pointer("/data/metadata/title").and_then(json_str) {
        fields.push(Field {
            key: "title",
            label: "Title",
            value: title,
        });
    }
    if let Some(status) = resp_json
        .pointer("/data/metadata/statusCode")
        .and_then(|v| v.as_u64())
    {
        fields.push(Field {
            key: "status_code",
            label: "Status",
            value: status.to_string(),
        });
    }
    if let Some(src) = resp_json
        .pointer("/data/metadata/sourceURL")
        .and_then(|v| v.as_str())
    {
        fields.push(Field {
            key: "source_url",
            label: "Source URL",
            value: src.to_string(),
        });
    }

    let output = format_summary(&url, &fields, markdown, truncated);
    Ok(serde_json::to_string(&ToolResult::success(output))?)
}

/// Firecrawl returns some metadata fields as either a string or an array of
/// strings; collapse to a single string for display.
fn json_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(a) => a.iter().find_map(|x| x.as_str()).map(|s| s.to_string()),
        _ => None,
    }
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        let fields = vec![
            Field {
                key: "title",
                label: "Title",
                value: "Example".into(),
            },
            Field {
                key: "status_code",
                label: "Status",
                value: "200".into(),
            },
            Field {
                key: "source_url",
                label: "Source URL",
                value: "https://example.com".into(),
            },
        ];
        format_summary("https://example.com", &fields, "# Example\n\nbody", false)
    }

    #[test]
    fn footer_is_present_and_last() {
        let out = sample();
        let idx = out.rfind("---").expect("footer separator present");
        let footer = &out[idx..];
        assert!(footer.contains("Data source: Firecrawl scrape API"));
        assert!(footer.contains("Do not infer or fabricate"));
        // Footer must be the final block — nothing of substance after it.
        assert!(out.trim_end().ends_with("not listed above."));
    }

    #[test]
    fn every_footer_field_appears_in_body() {
        let out = sample();
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
            // Each listed field is either rendered as a label or is the markdown block.
            let present = match field {
                "title" => body.contains("Title:"),
                "status_code" => body.contains("Status:"),
                "source_url" => body.contains("Source URL:"),
                "markdown" => body.contains("Content (markdown):"),
                other => panic!("unexpected footer field: {other}"),
            };
            assert!(present, "footer field '{field}' missing from body");
        }
    }

    #[test]
    fn absent_metadata_is_not_claimed() {
        // No metadata fields → footer lists only "markdown".
        let out = format_summary("https://x.test", &[], "content", false);
        let footer = &out[out.rfind("---").unwrap()..];
        assert!(footer.contains("Fields returned: markdown."));
        assert!(!footer.contains("title"));
        assert!(!footer.contains("status_code"));
    }

    #[test]
    fn truncation_is_disclosed() {
        let fields = vec![Field {
            key: "title",
            label: "Title",
            value: "T".into(),
        }];
        let out = format_summary("https://x.test", &fields, "abc", true);
        assert!(out.contains("truncated to"));
    }
}
