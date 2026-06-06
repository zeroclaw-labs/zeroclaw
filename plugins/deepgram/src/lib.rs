//! ZeroClaw WASM plugin: speech-to-text transcription via the Deepgram API.
//!
//! A stateless tool plugin — one request → one response, no polling. The caller
//! passes a URL to a hosted audio file; the plugin posts it to Deepgram's
//! pre-recorded `listen` endpoint and returns the transcript. Uses host
//! functions for the outbound HTTP request and to read the API key, so it needs
//! only the `http_client` and `env_read` permissions.
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

const LISTEN_URL: &str = "https://api.deepgram.com/v1/listen";
const API_KEY_ENV: &str = "DEEPGRAM_API_KEY";
const DEFAULT_MODEL: &str = "nova-3";

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

/// Percent-encode a query-parameter value (unreserved characters pass through).
fn query_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── Output formatting (fidelity footer required) ──────────────────

/// Build the model-facing transcript summary plus the mandatory fidelity
/// footer. Every field shown is read from the Deepgram response (except the
/// echoed input URL); the footer lists exactly those fields so the LLM cannot
/// invent data (speaker labels, word timings, sentiment) Deepgram did not
/// return in this output.
fn format_summary(
    audio_url: &str,
    transcript: &str,
    confidence: f64,
    model: &str,
    duration: Option<f64>,
    detected_language: Option<&str>,
) -> String {
    let duration_str = match duration {
        Some(d) => format!("{d:.2} s"),
        None => "not reported".to_string(),
    };

    let mut body = format!(
        "Transcript of: {audio_url}\n\
         Model: {model}, Confidence: {confidence:.3}, Duration: {duration_str}"
    );

    // `language` is only present when language detection ran, so it is listed
    // in the footer only when we actually have it.
    let mut fields = "audio_url, transcript, confidence, model, duration".to_string();
    if let Some(lang) = detected_language {
        body.push_str(&format!(", Detected language: {lang}"));
        fields.push_str(", detected_language");
    }

    body.push_str(&format!("\n\nText:\n{transcript}"));

    let footer = format!(
        "---\n\
         Data source: Deepgram speech-to-text API ({LISTEN_URL}).\n\
         Fields returned: {fields}.\n\
         Do not infer, estimate, or add fields that are not in this output."
    );

    format!("{body}\n\n{footer}")
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "deepgram_transcribe".into(),
        description: "Transcribe speech from a hosted audio file to text via the Deepgram API. \
             Use this to get a transcript of a podcast, voice note, recording, or any audio \
             file when you have its direct URL."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["audio_url"],
            "properties": {
                "audio_url": {
                    "type": "string",
                    "description": "Direct URL to the audio file (mp3, wav, m4a, flac, ogg, etc.)."
                },
                "model": {
                    "type": "string",
                    "description": "Deepgram model to use (e.g. 'nova-3', 'nova-2'). Default 'nova-3'."
                },
                "language": {
                    "type": "string",
                    "description": "BCP-47 language code (e.g. 'en', 'es'). Omit to use the model default; mutually exclusive with detect_language."
                },
                "detect_language": {
                    "type": "boolean",
                    "description": "Auto-detect the spoken language instead of assuming one. Default false."
                },
                "punctuate": {
                    "type": "boolean",
                    "description": "Add punctuation and capitalization. Default true."
                },
                "smart_format": {
                    "type": "boolean",
                    "description": "Apply Deepgram smart formatting (dates, numbers, etc.). Default true."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the Deepgram transcription tool.
#[plugin_fn]
pub fn execute(input: String) -> FnResult<String> {
    let args: serde_json::Value = serde_json::from_str(&input)?;

    // ── Parse + validate parameters ───────────────────────────────
    let audio_url = match args.get("audio_url").and_then(|v| v.as_str()) {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => return fail("Missing required parameter: 'audio_url'"),
    };
    if !(audio_url.starts_with("http://") || audio_url.starts_with("https://")) {
        return fail("Parameter 'audio_url' must be an http(s) URL to an audio file");
    }

    let model = args
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_MODEL);
    // Model identifiers are simple slugs; reject anything that could break the
    // query string.
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return fail(format!("Invalid model identifier '{model}'"));
    }

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let detect_language = args
        .get("detect_language")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if language.is_some() && detect_language {
        return fail("Set either 'language' or 'detect_language', not both");
    }

    let punctuate = args
        .get("punctuate")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let smart_format = args
        .get("smart_format")
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

    // ── Build query string and call Deepgram ──────────────────────
    let mut query = format!(
        "model={}&punctuate={}&smart_format={}",
        query_encode(model),
        punctuate,
        smart_format,
    );
    if let Some(lang) = language {
        query.push_str(&format!("&language={}", query_encode(lang)));
    }
    if detect_language {
        query.push_str("&detect_language=true");
    }

    let req = HttpRequest {
        method: "POST".into(),
        url: format!("{LISTEN_URL}?{query}"),
        headers: [
            ("Authorization".into(), format!("Token {api_key}")),
            ("Content-Type".into(), "application/json".into()),
        ]
        .into_iter()
        .collect(),
        body: Some(serde_json::to_string(&json!({ "url": audio_url }))?),
    };

    let resp = match http_request(&req) {
        Ok(r) => r,
        Err(e) => return fail(format!("Deepgram request failed: {e}")),
    };
    if resp.status >= 400 {
        return fail(format!(
            "Deepgram API error ({}): {}",
            resp.status,
            &resp.body[..resp.body.len().min(500)]
        ));
    }

    // ── Parse response ────────────────────────────────────────────
    let resp_json: serde_json::Value = serde_json::from_str(&resp.body)
        .map_err(|e| Error::msg(format!("failed to parse Deepgram response: {e}")))?;

    let channel = resp_json.pointer("/results/channels/0");
    let alternative = channel.and_then(|c| c.pointer("/alternatives/0"));

    let transcript = alternative
        .and_then(|a| a.get("transcript"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    if transcript.is_empty() {
        return fail("Deepgram returned an empty transcript for this audio");
    }

    let confidence = alternative
        .and_then(|a| a.get("confidence"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let duration = resp_json
        .pointer("/metadata/duration")
        .and_then(|v| v.as_f64());

    let detected_language = channel
        .and_then(|c| c.get("detected_language"))
        .and_then(|v| v.as_str());

    let output = format_summary(
        &audio_url,
        &transcript,
        confidence,
        model,
        duration,
        detected_language,
    );

    Ok(serde_json::to_string(&ToolResult::success(output))?)
}

fn fail(msg: impl Into<String>) -> FnResult<String> {
    Ok(serde_json::to_string(&ToolResult::failure(msg))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> String {
        format_summary(
            "https://example.com/audio.mp3",
            "hello world this is a test",
            0.991,
            "nova-3",
            Some(12.34),
            None,
        )
    }

    #[test]
    fn output_includes_fidelity_footer() {
        let out = sample_summary();
        assert!(out.contains("\n---\n"), "missing footer separator");
        assert!(out.contains("Data source:"), "missing data source line");
        assert!(
            out.contains("Fields returned:"),
            "missing fields-returned line"
        );
        assert!(out.contains("Do not infer"), "missing fidelity directive");
    }

    #[test]
    fn footer_lists_exactly_the_fields_in_the_body() {
        let out = sample_summary();
        assert!(out.contains("Transcript of: https://example.com/audio.mp3"));
        assert!(out.contains("Model: nova-3"));
        assert!(out.contains("Confidence: 0.991"));
        assert!(out.contains("Duration: 12.34 s"));
        assert!(out.contains("hello world this is a test"));
        assert!(
            out.contains("Fields returned: audio_url, transcript, confidence, model, duration.")
        );
    }

    #[test]
    fn footer_is_last_in_the_output() {
        let out = sample_summary();
        let footer_pos = out.find("---").expect("footer present");
        let body_end = out.find("Text:").expect("body present");
        assert!(footer_pos > body_end, "footer must come after the body");
        assert!(
            out.trim_end().ends_with("not in this output."),
            "fidelity directive must be the final line"
        );
    }

    #[test]
    fn detected_language_appears_only_when_present() {
        let without = sample_summary();
        assert!(!without.contains("detected_language"));
        assert!(!without.contains("Detected language:"));

        let with = format_summary(
            "https://example.com/a.wav",
            "hola mundo",
            0.95,
            "nova-3",
            Some(3.0),
            Some("es"),
        );
        assert!(with.contains("Detected language: es"));
        assert!(with.contains(
            "Fields returned: audio_url, transcript, confidence, model, duration, detected_language."
        ));
    }

    #[test]
    fn unreported_duration_renders_safely() {
        let out = format_summary("https://e.test/x.mp3", "hi", 0.5, "nova-2", None, None);
        assert!(out.contains("Duration: not reported"));
        assert!(out.contains("\n---\n"));
        assert!(out.contains("Do not infer"));
    }

    #[test]
    fn query_encode_escapes_reserved_characters() {
        assert_eq!(query_encode("en-US"), "en-US");
        assert_eq!(query_encode("a b&c"), "a%20b%26c");
    }
}
