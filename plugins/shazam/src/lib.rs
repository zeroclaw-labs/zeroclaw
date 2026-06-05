//! ZeroClaw WASM plugin: Shazam catalogue lookup via a RapidAPI Shazam service.
//!
//! Mirrors the native `ShazamTool` but runs as a sandboxed WASM plugin.
//! Uses host functions for HTTP requests and environment variable access.
//!
//! **Unofficial wrapper.** Shazam does not publish a free public API; this
//! plugin talks to a third-party service hosted on RapidAPI (default host
//! `shazam.p.rapidapi.com`). The wrapping service may rate-limit, change
//! response shapes, or sunset endpoints without notice — treat as best-effort.
//!
//! Two read actions are supported:
//! - `search_track` — search the Shazam catalogue by text query.
//! - `get_track_details` — fetch full metadata for a track by its Shazam key.
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `tool_metadata(_) -> JSON` — `{"name", "description", "parameters_schema"}`
//! - `execute(args_json) -> JSON` — `{"success", "output", "error?"}`
//!
//! **Host functions (provided by ZeroClaw runtime):**
//! - `zc_http_request(json) -> json` — HTTP request (needs `http_client`)
//! - `zc_env_read(name) -> value` — read an env var (needs `env_read`)
//!
//! ## Configuration
//!
//! The RapidAPI key is read from the `SHAZAM_RAPIDAPI_KEY` environment
//! variable. The RapidAPI host defaults to `shazam.p.rapidapi.com` and can be
//! overridden per call via the optional `host` argument.

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

const RAPIDAPI_KEY_ENV: &str = "SHAZAM_RAPIDAPI_KEY";
const DEFAULT_HOST: &str = "shazam.p.rapidapi.com";
const MAX_ERROR_BODY_CHARS: usize = 500;
const SEARCH_LIMIT_MIN: u64 = 1;
const SEARCH_LIMIT_MAX: u64 = 25;
const DEFAULT_LIMIT: u64 = 5;

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

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "shazam".into(),
        description: "Look up tracks in the Shazam catalogue via a RapidAPI Shazam \
                       service. search_track does a text search by title/artist; \
                       get_track_details fetches full metadata for a Shazam track key. \
                       Note: this is an unofficial third-party wrapper and may rate-\
                       limit or change shape without notice. Audio-fingerprint \
                       identification is not supported in v1. Requires the \
                       SHAZAM_RAPIDAPI_KEY environment variable."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["action"],
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
                },
                "host": {
                    "type": "string",
                    "description": "RapidAPI Shazam host override. Default: 'shazam.p.rapidapi.com'."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Shazam lookup tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate action ───────────────────────────────────
    let action = match args.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => {
            return Ok(serde_json::to_string(&ToolResult::failure(
                "Missing required parameter: action",
            ))?);
        }
    };
    if !matches!(action, "search_track" | "get_track_details") {
        return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "Unknown action: {action}. Valid actions: search_track, get_track_details"
        )))?);
    }

    let host = args
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_HOST);

    let locale = args
        .get("locale")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("en-US");

    // ── Validate args + build the path+query for the chosen action ─
    // Argument validation runs BEFORE the key read so a misuse (missing
    // query/track_key) returns a clean failure even when no key is set.
    let path_and_query = match action {
        "search_track" => {
            let query = match args.get("query").and_then(|v| v.as_str()) {
                Some(q) if !q.trim().is_empty() => q.trim().to_string(),
                _ => {
                    return Ok(serde_json::to_string(&ToolResult::failure(
                        "search_track requires query parameter",
                    ))?);
                }
            };
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_LIMIT)
                .clamp(SEARCH_LIMIT_MIN, SEARCH_LIMIT_MAX);
            format!(
                "/search?term={}&locale={}&offset=0&limit={limit}",
                urlencoding(&query),
                urlencoding(locale)
            )
        }
        "get_track_details" => {
            let track_key = match args.get("track_key").and_then(|v| v.as_str()) {
                Some(k) if !k.trim().is_empty() => k.trim().to_string(),
                _ => {
                    return Ok(serde_json::to_string(&ToolResult::failure(
                        "get_track_details requires track_key parameter",
                    ))?);
                }
            };
            format!(
                "/songs/get-details?key={}&locale={}",
                urlencoding(&track_key),
                urlencoding(locale)
            )
        }
        _ => unreachable!(),
    };

    // ── Read API key via host function ────────────────────────────
    // NOTE: when the env var is unset the host function returns an error,
    // which Extism surfaces as a plugin-call trap (not a value we can match).
    // The empty-string case below is the only one reachable as a value.
    let api_key = match env_read(RAPIDAPI_KEY_ENV) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        Ok(_) => {
            return Ok(serde_json::to_string(&ToolResult::failure(format!(
                "API key {RAPIDAPI_KEY_ENV} is empty"
            )))?);
        }
        Err(e) => {
            return Ok(serde_json::to_string(&ToolResult::failure(format!(
                "Missing API key: set the {RAPIDAPI_KEY_ENV} environment variable ({e})"
            )))?);
        }
    };

    // ── Call the RapidAPI Shazam service via host HTTP function ────
    let url = format!("https://{host}{path_and_query}");
    let req = HttpRequest {
        method: "GET".into(),
        url,
        headers: [
            ("X-RapidAPI-Key".into(), api_key),
            ("X-RapidAPI-Host".into(), host.to_string()),
        ]
        .into_iter()
        .collect(),
        body: None,
    };

    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return Ok(serde_json::to_string(&ToolResult::failure(format!(
                "Shazam {path_and_query} request failed: {e}"
            )))?);
        }
    };

    if resp.status >= 400 {
        return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "Shazam {path_and_query} failed ({}): {}",
            resp.status,
            truncate_chars(&resp.body, MAX_ERROR_BODY_CHARS)
        )))?);
    }

    // ── Parse + render the response (with fidelity footer) ────────
    let value: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Shazam response: {e}")))?;

    let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let output = format!("{pretty}{}", fidelity_footer(host, &value));

    Ok(serde_json::to_string(&ToolResult::success(output))?)
}

/// Build the mandatory fidelity footer: names the data source and lists the
/// top-level fields actually present in the response, so the consuming LLM
/// cannot fabricate fields that were never returned.
fn fidelity_footer(host: &str, value: &serde_json::Value) -> String {
    let fields = match value.as_object() {
        Some(map) if !map.is_empty() => map.keys().cloned().collect::<Vec<_>>().join(", "),
        Some(_) => "(empty object)".to_string(),
        None => match value {
            serde_json::Value::Array(_) => "(top-level array)".to_string(),
            _ => "(non-object response)".to_string(),
        },
    };
    format!("\n\n— Source: RapidAPI Shazam ({host}); fields returned: {fields}")
}

/// Truncate a string to at most `max` characters (not bytes) so multi-byte
/// UTF-8 sequences are never split.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Minimal application/x-www-form-urlencoded encoding for query-string values.
/// Avoids a `urlencoding` crate dependency for one helper.
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
