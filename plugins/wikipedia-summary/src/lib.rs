//! A ZeroClaw WIT tool plugin: `wikipedia_summary`.
//!
//! Given a topic title, it fetches the short summary from the Wikipedia REST API
//! and returns the extract text. It demonstrates the full WIT authoring path:
//! implementing the `tool` + `plugin-info` exports and calling the host
//! `http-request` import. No credentials are needed (the API is public), so this
//! only requests the `http_client` permission.
//!
//! Build:  rustup target add wasm32-wasip2
//!         cargo build --target wasm32-wasip2 --release
//!         cp target/wasm32-wasip2/release/wikipedia_summary.wasm wikipedia_summary.wasm

wit_bindgen::generate!({
    world: "tool-plugin",
    path: "../../wit/v0",
    features: ["plugins-wit-v0"],
});

use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfoGuest;
use exports::zeroclaw::plugin::tool::{Guest as ToolGuest, ToolResult};
use zeroclaw::plugin::host;

struct WikipediaSummary;

impl PluginInfoGuest for WikipediaSummary {
    fn plugin_name() -> String {
        "wikipedia-summary".to_string()
    }
    fn plugin_version() -> String {
        "0.1.0".to_string()
    }
}

impl ToolGuest for WikipediaSummary {
    fn name() -> String {
        "wikipedia_summary".to_string()
    }

    fn description() -> String {
        "Look up a short factual summary of a topic from Wikipedia. \
         Use for quick definitions or overviews of people, places, things, and concepts."
            .to_string()
    }

    fn parameters_schema() -> String {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "The topic/article title to summarize, e.g. \"WebAssembly\"."
                }
            },
            "required": ["title"]
        })
        .to_string()
    }

    fn execute(args: String) -> Result<ToolResult, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(&args).map_err(|e| format!("invalid arguments JSON: {e}"))?;
        let title = parsed
            .get("title")
            .and_then(|t| t.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| "missing required parameter 'title'".to_string())?;

        // Wikipedia REST summary endpoint. Titles use underscores for spaces and
        // are path-escaped for the characters that matter in a path segment.
        let slug = encode_title(title);
        let url = format!("https://en.wikipedia.org/api/rest_v1/page/summary/{slug}");

        // Call the host HTTP capability. Credentials (none needed here) would be
        // injected host-side; the guest never sees secret values.
        // Wikipedia's REST API requires a descriptive User-Agent and rejects
        // requests without one (HTTP 403).
        let headers = r#"{"User-Agent":"zeroclaw-wikipedia-summary/0.1 (+https://github.com/zeroclaw-labs/zeroclaw)","Accept":"application/json"}"#;
        let response = match host::http_request("GET", &url, headers, None, None) {
            Ok(response) => response,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("request failed: {error}")),
                });
            }
        };

        if response.status == 404 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("no Wikipedia page found for '{title}'")),
            });
        }
        if response.status != 200 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Wikipedia returned HTTP {}", response.status)),
            });
        }

        let body: serde_json::Value = serde_json::from_slice(&response.body)
            .map_err(|e| format!("invalid response JSON: {e}"))?;
        let extract = body
            .get("extract")
            .and_then(|e| e.as_str())
            .unwrap_or("(no summary available)");

        Ok(ToolResult {
            success: true,
            output: extract.to_string(),
            error: None,
        })
    }
}

/// Minimal path-segment encoding: spaces to underscores (Wikipedia convention),
/// then percent-encode the handful of characters that would break the path.
fn encode_title(title: &str) -> String {
    let underscored = title.replace(' ', "_");
    let mut out = String::with_capacity(underscored.len());
    for byte in underscored.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

export!(WikipediaSummary);
