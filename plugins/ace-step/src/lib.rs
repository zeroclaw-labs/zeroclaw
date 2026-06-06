//! ZeroClaw WASM plugin: text-to-music generation via a self-hosted ACE-Step server.
//!
//! Unlike the `suno` plugin (which proxies the user's *paid* Suno account through
//! the `gcui-art/suno-api` wrapper), this plugin targets a **self-hosted ACE-Step**
//! HTTP server (`ace-step/ACE-Step`, open weights). The user runs the model on their
//! own hardware, so there is **no vendor API key and no per-generation cost** — the
//! "own your stack, pay no one" complement to `suno`.
//!
//! ## Plugin protocol
//!
//! **Exports:**
//! - `tool_metadata(_) -> JSON` — returns `{"name", "description", "parameters_schema"}`
//! - `execute(args_json) -> JSON` — returns `{"success", "output", "error?"}`
//!
//! **Host functions (provided by ZeroClaw runtime):**
//! - `zc_http_request(json) -> json` — make an HTTP request (requires `http_client` permission)
//! - `zc_env_read(name) -> value` — read an env var (requires `env_read` permission)
//!
//! ## Configuration (all optional — sensible defaults for a local install)
//! - `ACE_STEP_URL`  — base URL of the self-hosted server (default `http://localhost:7865`)
//! - `ACE_STEP_PATH` — generation endpoint path (default `/generate`)
//! - `ACE_STEP_TOKEN` — optional `Bearer` token if the server is auth-protected
//!
//! ## Server contract
//! The plugin `POST`s `{"tags", "lyrics", "audio_duration"}` (the standard ACE-Step
//! inputs) and reads the generated audio back from the JSON response. It accepts the
//! common response shapes self-hosted setups produce: an audio **URL** (`audio_url` /
//! `url` / `data[0].url`) or **base64** audio (`audio_base64` / `audio`). The host's
//! HTTP bridge is text-only, so the server must return a URL or base64-in-JSON (not a
//! raw binary body).

use extism_pdk::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

const DEFAULT_BASE_URL: &str = "http://localhost:7865";
const DEFAULT_PATH: &str = "/generate";
const BASE_URL_ENV: &str = "ACE_STEP_URL";
const PATH_ENV: &str = "ACE_STEP_PATH";
const TOKEN_ENV: &str = "ACE_STEP_TOKEN";

const DEFAULT_DURATION: u32 = 30;
const MAX_DURATION: u32 = 240;

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

/// Read an optional env var, returning `None` when unset or empty.
fn env_opt(var_name: &str) -> Option<String> {
    unsafe { zc_env_read(var_name.to_string()) }
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── Pure helpers (unit-testable without the host) ─────────────────

/// Clamp/normalize the requested duration into the supported range.
fn normalize_duration(requested: Option<f64>) -> u32 {
    match requested {
        Some(d) if d >= 1.0 => (d as u32).min(MAX_DURATION),
        _ => DEFAULT_DURATION,
    }
}

/// Join a base URL and a path into a single endpoint URL.
fn build_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// Locate the generated audio in a (varied) ACE-Step server response.
/// Returns `(kind, value)` where `kind` is `"url"` or `"base64"`.
fn extract_audio(v: &serde_json::Value) -> Option<(&'static str, String)> {
    // Unambiguous URL fields.
    for ptr in [
        "/audio_url",
        "/url",
        "/data/0/url",
        "/output_url",
        "/audio/url",
    ] {
        if let Some(s) = v.pointer(ptr).and_then(|x| x.as_str())
            && !s.is_empty()
        {
            return Some(("url", s.to_string()));
        }
    }
    // Unambiguous base64 fields.
    for ptr in ["/audio_base64", "/audio_b64", "/data/0/audio_base64"] {
        if let Some(s) = v.pointer(ptr).and_then(|x| x.as_str())
            && !s.is_empty()
        {
            return Some(("base64", s.to_string()));
        }
    }
    // Ambiguous fields: classify by content.
    for ptr in ["/audio", "/output", "/data/0", "/result"] {
        if let Some(s) = v.pointer(ptr).and_then(|x| x.as_str()) {
            if s.starts_with("http://") || s.starts_with("https://") {
                return Some(("url", s.to_string()));
            }
            if s.len() > 256 {
                return Some(("base64", s.to_string()));
            }
        }
    }
    None
}

/// The mandatory output-fidelity footer (must be the LAST thing in any output).
fn fidelity_footer() -> String {
    "Data source: self-hosted ACE-Step server (ace-step/ACE-Step open-weights model).\n\
     Fields returned: tags, lyrics, duration, audio.\n\
     Do not infer or fabricate any audio content, lyrics, or metadata beyond what the server returned."
        .to_string()
}

/// Format the success summary, ending with the fidelity footer.
fn format_summary(
    base_url: &str,
    tags: &str,
    has_lyrics: bool,
    duration: u32,
    audio_kind: &str,
    audio_value: &str,
) -> String {
    let lyrics_line = if has_lyrics {
        "provided"
    } else {
        "instrumental"
    };
    let audio_line = match audio_kind {
        "url" => format!("Audio URL: {audio_value}"),
        _ => format!("Audio (base64): data:audio/wav;base64,{audio_value}"),
    };
    format!(
        "Music generated successfully.\n\
         Server: {base_url}\n\
         Style/tags: {tags}\n\
         Lyrics: {lyrics_line}\n\
         Duration: {duration}s\n\
         {audio_line}\n\
         \n\
         {}",
        fidelity_footer()
    )
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "generate_music".into(),
        description: "Generate music from a text prompt using a self-hosted ACE-Step \
                       open-weights model. Runs on your own hardware — no vendor API key or \
                       per-generation cost. Returns the generated audio (URL or base64)."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Style/genre/mood tags describing the music, comma-separated \
                                    (e.g. 'lo-fi hip hop, mellow piano, 90 bpm')."
                },
                "lyrics": {
                    "type": "string",
                    "description": "Optional lyrics, with structure tags like [verse]/[chorus]. \
                                    Omit or leave empty for an instrumental track."
                },
                "duration": {
                    "type": "number",
                    "description": "Desired audio length in seconds (1-240, default 30)."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the music generation tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse parameters ──────────────────────────────────────────
    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => {
            return Ok(serde_json::to_string(&ToolResult::failure(
                "Missing required parameter: 'prompt'",
            ))?);
        }
    };

    let lyrics = args
        .get("lyrics")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    let duration = normalize_duration(args.get("duration").and_then(|v| v.as_f64()));

    // ── Resolve self-hosted server config ─────────────────────────
    let base_url = env_opt(BASE_URL_ENV).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let path = env_opt(PATH_ENV).unwrap_or_else(|| DEFAULT_PATH.to_string());
    let url = build_url(&base_url, &path);

    let mut headers = std::collections::HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if let Some(token) = env_opt(TOKEN_ENV) {
        headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    }

    let body = json!({
        "tags": prompt,
        "lyrics": lyrics,
        "audio_duration": duration,
    });

    let req = HttpRequest {
        method: "POST".into(),
        url,
        headers,
        body: Some(serde_json::to_string(&body)?),
    };

    // ── Call the self-hosted server ───────────────────────────────
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => {
            return Ok(serde_json::to_string(&ToolResult::failure(format!(
                "ACE-Step request failed ({base_url}): {e}. \
                 Is the self-hosted server running? Configure {BASE_URL_ENV}/{PATH_ENV} if needed."
            )))?);
        }
    };

    if resp.status >= 400 {
        return Ok(serde_json::to_string(&ToolResult::failure(format!(
            "ACE-Step server error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        )))?);
    }

    // ── Parse response ───────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse ACE-Step response: {e}")))?;

    let (audio_kind, audio_value) = match extract_audio(&resp_json) {
        Some(a) => a,
        None => {
            return Ok(serde_json::to_string(&ToolResult::failure(format!(
                "ACE-Step response contained no audio URL or base64 audio. \
                 Raw response (truncated): {}",
                &resp.body[..resp.body.len().min(500)]
            )))?);
        }
    };

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(
            &base_url,
            &prompt,
            !lyrics.is_empty(),
            duration,
            audio_kind,
            &audio_value,
        ),
    ))?)
}

// ── Unit tests (pure helpers; host functions are not available here) ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_defaults_and_clamps() {
        assert_eq!(normalize_duration(None), DEFAULT_DURATION);
        assert_eq!(normalize_duration(Some(0.0)), DEFAULT_DURATION);
        assert_eq!(normalize_duration(Some(-5.0)), DEFAULT_DURATION);
        assert_eq!(normalize_duration(Some(45.0)), 45);
        assert_eq!(normalize_duration(Some(9999.0)), MAX_DURATION);
    }

    #[test]
    fn build_url_joins_correctly() {
        assert_eq!(
            build_url("http://localhost:7865", "/generate"),
            "http://localhost:7865/generate"
        );
        assert_eq!(
            build_url("http://localhost:7865/", "/generate"),
            "http://localhost:7865/generate"
        );
        assert_eq!(build_url("http://host", "generate"), "http://host/generate");
    }

    #[test]
    fn extract_audio_finds_url_fields() {
        let v = json!({"audio_url": "https://x/a.wav"});
        assert_eq!(
            extract_audio(&v),
            Some(("url", "https://x/a.wav".to_string()))
        );

        let v = json!({"data": [{"url": "http://x/b.mp3"}]});
        assert_eq!(
            extract_audio(&v),
            Some(("url", "http://x/b.mp3".to_string()))
        );
    }

    #[test]
    fn extract_audio_finds_base64_fields() {
        let v = json!({"audio_base64": "UklGRiQ="});
        assert_eq!(extract_audio(&v), Some(("base64", "UklGRiQ=".to_string())));
    }

    #[test]
    fn extract_audio_classifies_ambiguous_field() {
        // URL-looking value.
        let v = json!({"audio": "https://x/c.wav"});
        assert_eq!(
            extract_audio(&v),
            Some(("url", "https://x/c.wav".to_string()))
        );

        // Long string => treated as base64.
        let long = "A".repeat(300);
        let v = json!({"output": long});
        assert_eq!(extract_audio(&v), Some(("base64", "A".repeat(300))));
    }

    #[test]
    fn extract_audio_none_when_absent() {
        let v = json!({"status": "ok", "msg": "queued"});
        assert_eq!(extract_audio(&v), None);
    }

    #[test]
    fn footer_has_required_lines() {
        let f = fidelity_footer();
        assert!(f.contains("Data source:"));
        assert!(f.contains("Fields returned:"));
        assert!(f.contains("Do not infer"));
    }

    #[test]
    fn summary_ends_with_footer() {
        let out = format_summary(
            "http://localhost:7865",
            "lo-fi, piano",
            true,
            30,
            "url",
            "https://x/a.wav",
        );
        assert!(out.trim_end().ends_with(
            "Do not infer or fabricate any audio content, lyrics, or metadata beyond what the server returned."
        ));
        assert!(out.contains("Audio URL: https://x/a.wav"));
        assert!(out.contains("Lyrics: provided"));
    }

    #[test]
    fn summary_marks_instrumental_and_base64() {
        let out = format_summary(
            "http://localhost:7865",
            "ambient",
            false,
            60,
            "base64",
            "Ukl= ",
        );
        assert!(out.contains("Lyrics: instrumental"));
        assert!(out.contains("Duration: 60s"));
        assert!(out.contains("data:audio/wav;base64,"));
    }
}
