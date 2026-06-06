//! ZeroClaw WASM plugin: speech-to-text transcription via the AssemblyAI API.
//!
//! Transcription is asynchronous: this plugin submits a transcript request for
//! a hosted audio URL, then polls the transcript endpoint until the job reaches
//! a terminal state (`completed` or `error`) before returning the text. Mirrors
//! the submit-then-poll shape of the `video-gen-fal` plugin. Uses host functions
//! for HTTP requests and environment variable access.
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

const TRANSCRIPT_URL: &str = "https://api.assemblyai.com/v2/transcript";
const API_KEY_ENV: &str = "ASSEMBLYAI_API_KEY";

/// Number of times we poll the transcript endpoint before giving up.
const MAX_POLLS: u32 = 120;
/// Delay between status polls, in milliseconds. 120 polls × 3s ≈ 360s budget.
const POLL_INTERVAL_MS: u64 = 3_000;

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

fn auth_headers(api_key: &str) -> std::collections::HashMap<String, String> {
    [
        ("Authorization".into(), api_key.to_string()),
        ("Content-Type".into(), "application/json".into()),
    ]
    .into_iter()
    .collect()
}

// ── Output formatting (fidelity footer required) ──────────────────

/// Build the model-facing transcript summary plus the mandatory fidelity
/// footer. Every field shown is read from the AssemblyAI response (except the
/// echoed input URL); the footer lists exactly those fields so the LLM cannot
/// invent data (speaker labels, word timings, chapters, sentiment) that
/// AssemblyAI did not return in this output.
fn format_summary(
    audio_url: &str,
    transcript: &str,
    confidence: Option<f64>,
    audio_duration: Option<i64>,
    language_code: &str,
    id: &str,
) -> String {
    let confidence_str = match confidence {
        Some(c) => format!("{c:.3}"),
        None => "not reported".to_string(),
    };
    let duration_str = match audio_duration {
        Some(d) => format!("{d} s"),
        None => "not reported".to_string(),
    };

    let body = format!(
        "Transcript of: {audio_url}\n\
         Language: {language_code}, Confidence: {confidence_str}, Duration: {duration_str}\n\
         Transcript ID: {id}\n\
         \n\
         Text:\n{transcript}"
    );

    let footer = format!(
        "---\n\
         Data source: AssemblyAI speech-to-text API ({TRANSCRIPT_URL}).\n\
         Fields returned: audio_url, transcript, confidence, audio_duration, language_code, id.\n\
         Do not infer, estimate, or add fields that are not in this output."
    );

    format!("{body}\n\n{footer}")
}

// ── Plugin exports ────────────────────────────────────────────────

/// Export: returns tool metadata (name, description, parameters schema).
#[plugin_fn]
pub fn tool_metadata(_input: String) -> FnResult<String> {
    let meta = ToolMetadata {
        name: "assemblyai_transcribe".into(),
        description: "Transcribe speech from a hosted audio file to text via the AssemblyAI API. \
             Submits the audio URL, waits for the asynchronous job to finish, and returns \
             the transcript. Use this for a podcast, voice note, or recording when you have \
             its direct URL."
            .into(),
        parameters_schema: json!({
            "type": "object",
            "required": ["audio_url"],
            "properties": {
                "audio_url": {
                    "type": "string",
                    "description": "Direct URL to the audio file (mp3, wav, m4a, flac, etc.)."
                },
                "language_code": {
                    "type": "string",
                    "description": "Language of the audio (e.g. 'en', 'es', 'fr'). Omit to use the default; mutually exclusive with language_detection."
                },
                "language_detection": {
                    "type": "boolean",
                    "description": "Auto-detect the spoken language instead of assuming one. Default false."
                },
                "punctuate": {
                    "type": "boolean",
                    "description": "Add punctuation to the transcript. Default true."
                },
                "format_text": {
                    "type": "boolean",
                    "description": "Apply casing and text formatting. Default true."
                }
            }
        }),
    };
    Ok(serde_json::to_string(&meta)?)
}

/// Export: execute the AssemblyAI transcription tool.
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

    let language_code = args
        .get("language_code")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let language_detection = args
        .get("language_detection")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if language_code.is_some() && language_detection {
        return fail("Set either 'language_code' or 'language_detection', not both");
    }

    let punctuate = args
        .get("punctuate")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let format_text = args
        .get("format_text")
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

    // ── Submit the transcription job ──────────────────────────────
    let mut payload = json!({
        "audio_url": audio_url,
        "punctuate": punctuate,
        "format_text": format_text,
    });
    if let Some(lang) = language_code {
        payload["language_code"] = json!(lang);
    }
    if language_detection {
        payload["language_detection"] = json!(true);
    }

    let submit_req = HttpRequest {
        method: "POST".into(),
        url: TRANSCRIPT_URL.into(),
        headers: auth_headers(&api_key),
        body: Some(serde_json::to_string(&payload)?),
    };

    let submit_resp = match http_request(&submit_req) {
        Ok(r) => r,
        Err(e) => return fail(format!("AssemblyAI request failed: {e}")),
    };
    if submit_resp.status >= 400 {
        return fail(format!(
            "AssemblyAI API error ({}): {}",
            submit_resp.status,
            &submit_resp.body[..submit_resp.body.len().min(500)]
        ));
    }

    let submit_json: serde_json::Value = serde_json::from_str(&submit_resp.body)
        .map_err(|e| Error::msg(format!("failed to parse submit response: {e}")))?;

    let id = submit_json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::msg("submit response missing 'id'"))?
        .to_string();

    // ── Poll until the job reaches a terminal state ───────────────
    let poll_url = format!("{TRANSCRIPT_URL}/{id}");
    for _ in 0..MAX_POLLS {
        let poll_req = HttpRequest {
            method: "GET".into(),
            url: poll_url.clone(),
            headers: auth_headers(&api_key),
            body: None,
        };

        let poll_resp = match http_request(&poll_req) {
            Ok(r) => r,
            Err(e) => {
                return fail(format!("AssemblyAI status request failed (id {id}): {e}"));
            }
        };
        if poll_resp.status >= 400 {
            return fail(format!(
                "AssemblyAI API error ({}) for id {id}: {}",
                poll_resp.status,
                &poll_resp.body[..poll_resp.body.len().min(500)]
            ));
        }

        let poll_json: serde_json::Value = serde_json::from_str(&poll_resp.body)
            .map_err(|e| Error::msg(format!("failed to parse status response: {e}")))?;

        match poll_json.get("status").and_then(|v| v.as_str()) {
            Some("completed") => {
                return Ok(serde_json::to_string(&ToolResult::success(
                    build_completed_output(&audio_url, &id, &poll_json),
                ))?);
            }
            Some("error") => {
                let msg = poll_json
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                return fail(format!("AssemblyAI transcription failed (id {id}): {msg}"));
            }
            Some("queued") | Some("processing") => {
                std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
            }
            Some(other) => {
                return fail(format!(
                    "AssemblyAI returned unexpected status '{other}' for id {id}"
                ));
            }
            None => {
                std::thread::sleep(std::time::Duration::from_millis(POLL_INTERVAL_MS));
            }
        }
    }

    let total_s = (MAX_POLLS as u64 * POLL_INTERVAL_MS) / 1000;
    fail(format!(
        "AssemblyAI job did not complete within {MAX_POLLS} polls ({total_s}s total). \
         Transcript ID: {id}"
    ))
}

/// Assemble the success output from a completed transcript payload.
fn build_completed_output(audio_url: &str, id: &str, completed: &serde_json::Value) -> String {
    let transcript = completed
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let confidence = completed.get("confidence").and_then(|v| v.as_f64());
    let audio_duration = completed.get("audio_duration").and_then(|v| v.as_i64());
    let language_code = completed
        .get("language_code")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    format_summary(
        audio_url,
        &transcript,
        confidence,
        audio_duration,
        language_code,
        id,
    )
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
            Some(0.974),
            Some(42),
            "en",
            "abc-123",
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
        assert!(out.contains("Language: en"));
        assert!(out.contains("Confidence: 0.974"));
        assert!(out.contains("Duration: 42 s"));
        assert!(out.contains("Transcript ID: abc-123"));
        assert!(out.contains("hello world this is a test"));
        assert!(out.contains(
            "Fields returned: audio_url, transcript, confidence, audio_duration, language_code, id."
        ));
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
    fn unreported_optional_fields_render_safely() {
        let out = format_summary("https://e.test/x.mp3", "hi", None, None, "unknown", "id-1");
        assert!(out.contains("Confidence: not reported"));
        assert!(out.contains("Duration: not reported"));
        assert!(out.contains("\n---\n"));
        assert!(out.contains("Do not infer"));
    }

    #[test]
    fn build_completed_output_reads_response_fields() {
        let completed = json!({
            "status": "completed",
            "text": "  transcribed words  ",
            "confidence": 0.88,
            "audio_duration": 7,
            "language_code": "es"
        });
        let out = build_completed_output("https://e.test/a.wav", "xyz", &completed);
        assert!(out.contains("transcribed words"));
        assert!(out.contains("Language: es"));
        assert!(out.contains("Confidence: 0.880"));
        assert!(out.contains("Duration: 7 s"));
        assert!(out.contains("Transcript ID: xyz"));
    }
}
