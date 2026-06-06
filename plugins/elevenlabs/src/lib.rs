//! ZeroClaw WASM plugin: text-to-speech via the ElevenLabs API.
//!
//! A stateless tool plugin — one request → one response, no stored state. The
//! ElevenLabs TTS endpoint returns **binary audio**, so this plugin relies on
//! the host's base64 HTTP support (`body_base64` on the response): it reads the
//! audio bytes back as base64 and returns them as an `audio/mpeg` data URI for
//! downstream playback. Needs only the `http_client` and `env_read` permissions.
//!
//! Requires a host that supports `body_base64` on `zc_http_request` responses
//! (ZeroClaw #7288). On an older host the audio comes back as an (empty/garbled)
//! text body and the plugin reports a clear error rather than returning junk.
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

const TTS_BASE: &str = "https://api.elevenlabs.io/v1/text-to-speech/";
const OUTPUT_FORMAT: &str = "mp3_44100_128";
const API_KEY_ENV: &str = "ELEVENLABS_API_KEY";
const DEFAULT_MODEL: &str = "eleven_multilingual_v2";
/// ElevenLabs' documented example voice — a safe default when the caller does
/// not specify one. Override per call with the `voice_id` argument.
const DEFAULT_VOICE: &str = "JBFqnCBsd6RMkjVDRZzb";
/// Bound the input so a single call can't synthesize huge audio (the base64
/// data URI is returned inline). ElevenLabs accepts more, but this keeps tool
/// output sane.
const MAX_TEXT_CHARS: usize = 2_000;

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

/// Mirrors the host response, including the `body_base64` field added in #7288
/// that carries a binary response body.
#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    #[serde(default)]
    body: String,
    #[serde(default)]
    body_base64: Option<String>,
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

/// Rough decoded byte count of a standard-base64 string (4 chars → 3 bytes,
/// minus padding). Used only for a human-readable size note.
fn approx_bytes(b64: &str) -> usize {
    let pad = b64.bytes().rev().take_while(|&b| b == b'=').count();
    (b64.len() / 4) * 3 - pad
}

/// Build the model-facing output: a header, the audio as an `audio/mpeg` data
/// URI, and the mandatory fidelity footer (last, naming the source and the exact
/// fields present).
fn format_summary(voice_id: &str, model_id: &str, audio_b64: &str) -> String {
    let mut out = format!(
        "Generated speech audio.\n\
         Voice: {voice_id}\n\
         Model: {model_id}\n\
         Format: {OUTPUT_FORMAT} (mp3)\n\
         Audio size: ~{} bytes\n\n\
         Audio (data URI):\ndata:audio/mpeg;base64,{audio_b64}",
        approx_bytes(audio_b64)
    );
    out.push_str("\n\n---\n");
    out.push_str("Data source: ElevenLabs text-to-speech API (https://api.elevenlabs.io/v1/text-to-speech).\n");
    out.push_str("Fields returned: voice_id, model_id, audio.\n");
    out.push_str("Do not infer or fabricate any field not listed above.");
    out
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "text_to_speech".into(),
        description: "Convert text to spoken audio (mp3) using the ElevenLabs API, returned as an \
             audio/mpeg data URI for playback. Use this to generate a voice/audio rendering of \
             text."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to synthesize into speech (max 2000 characters)."
                },
                "voice_id": {
                    "type": "string",
                    "description": "ElevenLabs voice id (alphanumeric). Defaults to a standard voice."
                },
                "model_id": {
                    "type": "string",
                    "description": "ElevenLabs model id (default 'eleven_multilingual_v2')."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the ElevenLabs text-to-speech tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return fail("Missing required parameter: 'text'"),
    };
    if text.chars().count() > MAX_TEXT_CHARS {
        return fail(format!(
            "'text' is too long ({} chars); maximum is {MAX_TEXT_CHARS}",
            text.chars().count()
        ));
    }
    let voice_id = args
        .get("voice_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_VOICE)
        .to_string();
    // voice_id goes straight into the request URL path.
    if !voice_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return fail(format!(
            "Invalid voice_id '{voice_id}': must be alphanumeric (an ElevenLabs voice id)"
        ));
    }
    let model_id = args
        .get("model_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL)
        .to_string();

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

    // ── Call ElevenLabs via host HTTP function ────────────────────
    let body = json!({ "text": text, "model_id": model_id });
    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{TTS_BASE}{voice_id}?output_format={OUTPUT_FORMAT}"),
        headers: [
            ("xi-api-key".into(), api_key),
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "audio/mpeg".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&body)?),
    };
    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("ElevenLabs request failed: {e}")),
    };
    if resp.status >= 400 {
        // Errors come back as a JSON/text body, not audio.
        return fail(format!(
            "ElevenLabs API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Read the binary audio (base64) ────────────────────────────
    let audio_b64 = match resp.body_base64 {
        Some(b64) if !b64.is_empty() => b64,
        _ => {
            return fail(
                "ElevenLabs returned no binary audio. The host may lack base64 HTTP response \
                 support (requires ZeroClaw #7288); upgrade the gateway/runtime to use this tool.",
            );
        }
    };

    Ok(serde_json::to_string(&ToolResult::success(
        format_summary(&voice_id, &model_id, &audio_b64),
    ))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_present_last_and_lists_fields() {
        let out = format_summary("Voice1", "eleven_multilingual_v2", "QUJD");
        let (body, footer) = out.split_once("---").unwrap();
        assert!(footer.contains("Data source: ElevenLabs text-to-speech API"));
        assert!(footer.contains("Fields returned: voice_id, model_id, audio."));
        assert!(out.trim_end().ends_with("not listed above."));
        // Every footer field appears in the body.
        assert!(body.contains("Voice: Voice1"));
        assert!(body.contains("Model: eleven_multilingual_v2"));
        assert!(body.contains("data:audio/mpeg;base64,QUJD"));
    }

    #[test]
    fn audio_returned_as_data_uri() {
        let out = format_summary("v", "m", "QUJD");
        assert!(out.contains("data:audio/mpeg;base64,QUJD"));
    }

    #[test]
    fn approx_bytes_handles_padding() {
        // "QUJD" -> "ABC" (3 bytes, no padding)
        assert_eq!(approx_bytes("QUJD"), 3);
        // "QUI=" -> "AB" (2 bytes, 1 pad)
        assert_eq!(approx_bytes("QUI="), 2);
        // "QQ==" -> "A" (1 byte, 2 pad)
        assert_eq!(approx_bytes("QQ=="), 1);
    }
}
