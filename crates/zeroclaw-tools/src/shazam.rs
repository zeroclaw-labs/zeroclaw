//! Shazam tool — track lookup via a RapidAPI Shazam service.
//!
//! **Unofficial wrapper.** Shazam does not publish a free public API;
//! this tool talks to a third-party service hosted on RapidAPI (default
//! host `shazam.p.rapidapi.com`). The wrapping service may rate-limit,
//! change response shapes, or sunset endpoints without notice — treat
//! as best-effort.
//!
//! Two read actions are supported:
//!
//! - `search_track` — search the Shazam catalogue by text query.
//! - `get_track_details` — fetch full metadata for a track by its
//!   Shazam track key (returned by `search_track`).
//!
//! Audio-fingerprint identification is intentionally out of scope for
//! v1 — the multipart/audio surface is the most fragile path on a
//! third-party wrapper and ships in a follow-up if there's demand.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

const MAX_ERROR_BODY_CHARS: usize = 500;
const SEARCH_LIMIT_MIN: u64 = 1;
const SEARCH_LIMIT_MAX: u64 = 25;

/// Tool for interacting with a RapidAPI Shazam service.
pub struct ShazamTool {
    rapidapi_key: String,
    rapidapi_host: String,
    request_timeout_secs: u64,
    http: reqwest::Client,
    security: Arc<SecurityPolicy>,
}

impl ShazamTool {
    pub fn new(
        rapidapi_key: String,
        rapidapi_host: String,
        request_timeout_secs: u64,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        let rapidapi_host = rapidapi_host.trim().to_string();
        Self {
            rapidapi_key,
            rapidapi_host,
            request_timeout_secs,
            http: reqwest::Client::new(),
            security,
        }
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs.max(1))
    }

    fn headers(&self) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "X-RapidAPI-Key",
            self.rapidapi_key
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid RapidAPI key header: {e}"))?,
        );
        headers.insert(
            "X-RapidAPI-Host",
            self.rapidapi_host
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid RapidAPI host header: {e}"))?,
        );
        Ok(headers)
    }

    fn base_url(&self) -> String {
        format!("https://{}", self.rapidapi_host)
    }

    async fn get_json(&self, path_and_query: &str) -> anyhow::Result<Value> {
        let url = format!("{}{path_and_query}", self.base_url());
        let resp = self
            .http
            .get(&url)
            .headers(self.headers()?)
            .timeout(self.timeout())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let truncated =
                crate::util_helpers::truncate_with_ellipsis(&body, MAX_ERROR_BODY_CHARS);
            anyhow::bail!("Shazam {path_and_query} failed ({status}): {truncated}");
        }
        resp.json().await.map_err(Into::into)
    }
}

#[async_trait]
impl Tool for ShazamTool {
    fn name(&self) -> &str {
        "shazam"
    }

    fn description(&self) -> &str {
        "Look up tracks in the Shazam catalogue via a RapidAPI Shazam \
         service. search_track does a text search by title/artist; \
         get_track_details fetches full metadata for a Shazam track key. \
         Note: this is an unofficial third-party wrapper and may rate-\
         limit or change shape without notice. Audio-fingerprint \
         identification is not supported in v1."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["search_track", "get_track_details"],
                    "description": "The Shazam operation to perform."
                },
                "query": {
                    "type": "string",
                    "description": "Text query (e.g. 'shape of you ed sheeran'). Required for search_track."
                },
                "limit": {
                    "type": "integer",
                    "minimum": SEARCH_LIMIT_MIN,
                    "maximum": SEARCH_LIMIT_MAX,
                    "description": "Max results for search_track (1-25). Default: 5."
                },
                "track_key": {
                    "type": "string",
                    "description": "Shazam track key (returned by search_track). Required for get_track_details."
                },
                "locale": {
                    "type": "string",
                    "description": "BCP-47 locale (e.g. 'en-US'). Default: 'en-US'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: action".into()),
                });
            }
        };

        if !matches!(action, "search_track" | "get_track_details") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: {action}. Valid actions: search_track, get_track_details"
                )),
            });
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "shazam")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let locale = args
            .get("locale")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("en-US");

        let result = match action {
            "search_track" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) if !q.trim().is_empty() => q.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("search_track requires query parameter".into()),
                        });
                    }
                };
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5)
                    .clamp(SEARCH_LIMIT_MIN, SEARCH_LIMIT_MAX);
                self.get_json(&format!(
                    "/search?term={}&locale={}&offset=0&limit={limit}",
                    urlencoding(&query),
                    urlencoding(locale)
                ))
                .await
            }
            "get_track_details" => {
                let track_key = match args.get("track_key").and_then(|v| v.as_str()) {
                    Some(k) if !k.trim().is_empty() => k.trim().to_string(),
                    _ => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("get_track_details requires track_key parameter".into()),
                        });
                    }
                };
                self.get_json(&format!(
                    "/songs/get-details?key={}&locale={}",
                    urlencoding(&track_key),
                    urlencoding(locale)
                ))
                .await
            }
            _ => unreachable!(),
        };

        match result {
            Ok(value) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

/// Minimal application/x-www-form-urlencoded encoding for query-string
/// values. Avoids a `urlencoding` crate dependency for one helper.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.bytes() {
        match ch {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(ch as char);
            }
            _ => {
                out.push_str(&format!("%{ch:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool() -> ShazamTool {
        ShazamTool::new(
            "test-key".into(),
            "shazam.p.rapidapi.com".into(),
            15,
            Arc::new(SecurityPolicy::default()),
        )
    }

    #[test]
    fn name_is_shazam() {
        assert_eq!(test_tool().name(), "shazam");
    }

    #[test]
    fn description_warns_unofficial() {
        let tool = test_tool();
        let d = tool.description();
        assert!(d.to_lowercase().contains("unofficial"));
    }

    #[test]
    fn parameters_schema_requires_action() {
        let schema = test_tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[test]
    fn parameters_schema_lists_actions() {
        let schema = test_tool().parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        for expected in &["search_track", "get_track_details"] {
            assert!(names.contains(expected), "missing action: {expected}");
        }
    }

    #[test]
    fn parameters_schema_limit_bounds() {
        let schema = test_tool().parameters_schema();
        let limit = &schema["properties"]["limit"];
        assert_eq!(limit["minimum"], SEARCH_LIMIT_MIN);
        assert_eq!(limit["maximum"], SEARCH_LIMIT_MAX);
    }

    #[test]
    fn base_url_uses_configured_host() {
        let tool = ShazamTool::new(
            "k".into(),
            "  custom.host.example  ".into(),
            15,
            Arc::new(SecurityPolicy::default()),
        );
        assert_eq!(tool.base_url(), "https://custom.host.example");
    }

    #[test]
    fn headers_include_rapidapi_key_and_host() {
        let tool = test_tool();
        let headers = tool.headers().unwrap();
        assert_eq!(headers.get("X-RapidAPI-Key").unwrap(), "test-key");
        assert_eq!(
            headers.get("X-RapidAPI-Host").unwrap(),
            "shazam.p.rapidapi.com"
        );
    }

    #[test]
    fn urlencoding_handles_unsafe_chars() {
        assert_eq!(urlencoding("ed sheeran"), "ed%20sheeran");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoding("ABCabc019-_.~"), "ABCabc019-_.~");
    }

    #[tokio::test]
    async fn execute_missing_action_returns_error() {
        let result = test_tool().execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("action"));
    }

    #[tokio::test]
    async fn execute_unknown_action_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "nope"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn execute_search_missing_query_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "search_track"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("query"));
    }

    #[tokio::test]
    async fn execute_get_track_details_missing_key_returns_error() {
        let result = test_tool()
            .execute(json!({"action": "get_track_details"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("track_key"));
    }

    #[test]
    fn spec_reflects_metadata() {
        let tool = test_tool();
        let spec = tool.spec();
        assert_eq!(spec.name, "shazam");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}
