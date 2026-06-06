//! ZeroClaw WASM plugin: AI music generation via a self-hosted Suno API.
//!
//! A stateless tool plugin — one `execute` call generates a song and returns its
//! hosted audio URL(s). It targets the **open-source, self-hosted**
//! `gcui-art/suno-api` wrapper (the user runs their own instance against their
//! own Suno account), so there is no vendor key or lock-in: the plugin only needs
//! the wrapper's base URL. Output is a URL (text), so no binary host support is
//! required. Needs only the `http_client` and `env_read` permissions.
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

/// Base URL of the self-hosted `gcui-art/suno-api` instance.
const API_URL_ENV: &str = "SUNO_API_URL";
const DEFAULT_BASE: &str = "http://localhost:3000";
/// Optional bearer token if the user put their wrapper behind auth.
const API_TOKEN_ENV: &str = "SUNO_API_TOKEN";
/// Bounded GET poll fallback if `wait_audio` returned before the audio was ready.
/// No sleep host-fn exists, so spacing comes from each request's round-trip.
const MAX_POLLS: usize = 12;

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

/// A generated clip with a ready audio URL.
struct Clip {
    title: String,
    url: String,
}

/// Extract clips that already have a non-empty `audio_url` from a Suno response
/// array. Returns `(ready_clips, all_ids)`.
fn parse_clips(value: &serde_json::Value) -> (Vec<Clip>, Vec<String>) {
    let mut ready = Vec::new();
    let mut ids = Vec::new();
    if let Some(arr) = value.as_array() {
        for c in arr {
            if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
                ids.push(id.to_string());
            }
            let url = c.get("audio_url").and_then(|v| v.as_str()).unwrap_or("");
            if !url.is_empty() {
                let title = c
                    .get("title")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Untitled")
                    .to_string();
                ready.push(Clip {
                    title,
                    url: url.to_string(),
                });
            }
        }
    }
    (ready, ids)
}

/// Build the model-facing output and the mandatory fidelity footer (last,
/// naming the source and listing exactly the fields present).
fn format_summary(prompt: &str, clips: &[Clip]) -> String {
    let mut out = format!("Generated music for: {prompt} ({} clip(s))\n", clips.len());
    for (i, c) in clips.iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, c.title, c.url));
    }
    out.push_str("\n---\n");
    out.push_str("Data source: self-hosted Suno API (gcui-art/suno-api).\n");
    out.push_str("Fields returned: prompt, clips.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "generate_music".into(),
        description:
            "Generate an original song from a text prompt using a self-hosted Suno API instance \
             (open-source gcui-art/suno-api). Returns hosted audio URL(s) for the generated \
             clip(s). Set SUNO_API_URL to your instance."
                .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Description of the music to generate (mood, genre, lyrics theme)."
                },
                "instrumental": {
                    "type": "boolean",
                    "description": "Generate an instrumental track with no vocals (default false)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Suno music generation tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse parameters ──────────────────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return fail("Missing required parameter: 'prompt'"),
    };
    let instrumental = args
        .get("instrumental")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // ── Resolve the self-hosted base URL (defaults to localhost) ──
    let base = match env_read(API_URL_ENV) {
        Ok(u) if !u.trim().is_empty() => u.trim().trim_end_matches('/').to_string(),
        _ => DEFAULT_BASE.to_string(),
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return fail(format!(
            "Invalid {API_URL_ENV} '{base}': must be an http(s) URL to your self-hosted suno-api"
        ));
    }

    // Optional bearer token if the wrapper is behind auth.
    let mut headers: std::collections::HashMap<String, String> =
        [("Content-Type".to_string(), "application/json".to_string())]
            .into_iter()
            .collect();
    if let Ok(tok) = env_read(API_TOKEN_ENV)
        && !tok.trim().is_empty()
    {
        headers.insert("Authorization".into(), format!("Bearer {}", tok.trim()));
    }

    // ── Submit the generation (wait_audio lets the wrapper block) ─
    let body = json!({ "prompt": prompt, "make_instrumental": instrumental, "wait_audio": true });
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{base}/api/generate"),
        headers: headers.clone(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return fail(format!(
                "Suno request failed: {e}. Is your self-hosted suno-api running at {base}?"
            ));
        }
    };
    if resp.status >= 400 {
        return fail(format!(
            "Suno API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Suno response: {e}")))?;
    let (mut clips, ids) = parse_clips(&resp_json);

    // ── Poll fallback if the audio wasn't ready yet ───────────────
    if clips.is_empty() && !ids.is_empty() {
        let get_url = format!("{base}/api/get?ids={}", percent_encode(&ids.join(",")));
        let mut polls = 0;
        while clips.is_empty() && polls < MAX_POLLS {
            let poll = HttpRequest {
                method: "GET".into(),
                url: get_url.clone(),
                headers: headers.clone(),
                body: None,
            };
            match http_request(&poll) {
                Ok(r) if r.status < 400 => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.body) {
                        clips = parse_clips(&v).0;
                    }
                }
                _ => break,
            }
            polls += 1;
        }
    }

    if clips.is_empty() {
        return fail(
            "Suno generation is still in progress (no audio URL yet). Try again shortly, or \
             check your self-hosted suno-api instance.",
        );
    }

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&prompt, &clips),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clips_extracts_ready_audio_and_ids() {
        let v = json!([
            {"id": "a", "status": "submitted", "audio_url": ""},
            {"id": "b", "status": "streaming", "audio_url": "https://s.test/b.mp3", "title": "Song B"}
        ]);
        let (ready, ids) = parse_clips(&v);
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].title, "Song B");
        assert_eq!(ready[0].url, "https://s.test/b.mp3");
    }

    #[test]
    fn footer_present_last_lists_fields() {
        let clips = vec![Clip {
            title: "T".into(),
            url: "https://s.test/a.mp3".into(),
        }];
        let out = format_summary("lofi beat", &clips);
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: self-hosted Suno API"));
        assert!(footer.contains("Fields returned: prompt, clips."));
        assert!(out.trim_end().ends_with("not listed above."));
        assert!(body.contains("Generated music for: lofi beat"));
        assert!(body.contains("https://s.test/a.mp3"));
    }

    #[test]
    fn percent_encode_basics() {
        assert_eq!(percent_encode("a,b"), "a%2Cb");
        assert_eq!(percent_encode("id-1_2"), "id-1_2");
    }
}
