use anyhow::{bail, Context, Result};

use crate::config::TranscriptionConfig;
use crate::providers::resolve_provider_credential;

/// Maximum upload size for Gemini API (20 MB).
const MAX_AUDIO_BYTES: usize = 20 * 1024 * 1024;

/// Normalize a value - trim whitespace and filter empty strings.
fn normalize_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Get the API key for transcription using the same logic as the Gemini provider.
/// Priority: config.api_key > resolve_provider_credential("gemini", None) > generic fallbacks
fn get_transcription_api_key(config: &TranscriptionConfig) -> Option<String> {
    // First try config.api_key
    if let Some(ref key) = config.api_key {
        if let Some(normalized) = normalize_non_empty(key) {
            return Some(normalized);
        }
    }
    // Then use the centralized provider credential resolution (supports GEMINI_API_KEY, GOOGLE_API_KEY, ZEROCLAW_API_KEY, API_KEY)
    resolve_provider_credential("gemini", None)
}

/// Check which transcription provider to use.
///
/// Priority:
/// 1. If api_url is explicitly set (non-default) → use that provider
/// 2. If model contains "whisper" → use Groq
/// 3. Default → use Gemini
fn get_transcription_provider(config: &TranscriptionConfig) -> TranscriptionProvider {
    // Check if api_url is explicitly set to a custom value
    // Default is "https://api.groq.com/openai/v1/audio/transcriptions"
    let default_groq_url = "https://api.groq.com/openai/v1/audio/transcriptions";

    if !config.api_url.is_empty() && config.api_url != default_groq_url {
        // Custom URL set - use Groq (for now)
        if config.api_url.contains("groq.com") {
            tracing::debug!("Using Groq: custom api_url set");
        } else {
            tracing::debug!("Using custom URL: {}", config.api_url);
        }
        return TranscriptionProvider::Groq;
    }

    // If model contains "whisper", use Groq
    if config.model.to_lowercase().contains("whisper") {
        tracing::debug!("Using Groq Whisper: model contains 'whisper'");
        return TranscriptionProvider::Groq;
    }

    // Default to Gemini (api_url is default Groq URL or empty → use Gemini)
    tracing::debug!("Using Gemini for transcription: default provider");
    TranscriptionProvider::Gemini
}

/// Transcription provider enum
#[derive(Debug, Clone, Copy, PartialEq)]
enum TranscriptionProvider {
    Gemini,
    Groq,
}

/// Transcribe audio bytes using Gemini API.
///
/// Returns the transcribed text on success.
pub async fn transcribe_audio(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
) -> Result<String> {
    if audio_data.len() > MAX_AUDIO_BYTES {
        bail!(
            "Audio file too large ({} bytes, max {MAX_AUDIO_BYTES})",
            audio_data.len()
        );
    }

    // Check which provider to use based on config
    let provider = get_transcription_provider(config);

    match provider {
        TranscriptionProvider::Gemini => {
            transcribe_audio_gemini(audio_data, file_name, config).await
        }
        TranscriptionProvider::Groq => transcribe_audio_groq(audio_data, file_name, config).await,
    }
}

/// Transcribe audio using Gemini API.
async fn transcribe_audio_gemini(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
) -> Result<String> {
    // Get API key using the same logic as Gemini provider
    let api_key = get_transcription_api_key(config)
        .context("Missing transcription API key: set [transcription].api_key, GEMINI_API_KEY, or GOOGLE_API_KEY")?;

    // Determine MIME type from file extension
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .unwrap_or("")
        .to_lowercase();

    let mime_type = match extension.as_str() {
        "mp3" | "mpeg" => "audio/mpeg",
        "mp4" | "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "webm" => "audio/webm",
        "flac" => "audio/flac",
        _ => "audio/mpeg", // default
    };

    // Encode audio to base64
    let audio_base64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &audio_data);

    // Use the configured model or default
    let model = if config.model.is_empty() {
        "gemini-2.0-flash-exp".to_string()
    } else {
        config.model.clone()
    };

    // Build Gemini API URL
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let client = crate::config::build_runtime_proxy_client("transcription.gemini");

    // Build request with inline audio data
    #[derive(serde::Serialize)]
    struct GeminiRequest {
        contents: Vec<Content>,
        system_instruction: SystemInstruction,
    }

    #[derive(serde::Serialize)]
    struct Content {
        parts: Vec<Part>,
    }

    #[derive(serde::Serialize)]
    struct Part {
        text: Option<String>,
        inline_data: Option<InlineData>,
    }

    #[derive(serde::Serialize)]
    struct InlineData {
        mime_type: String,
        data: String,
    }

    #[derive(serde::Serialize)]
    struct SystemInstruction {
        parts: Vec<SystemPart>,
    }

    #[derive(serde::Serialize)]
    struct SystemPart {
        text: String,
    }

    // Send audio only - system instruction handles the prompt
    let request = GeminiRequest {
        contents: vec![Content {
            parts: vec![Part {
                inline_data: Some(InlineData {
                    mime_type: mime_type.to_string(),
                    data: audio_base64,
                }),
                text: None,
            }],
        }],
        system_instruction: SystemInstruction {
            parts: vec![SystemPart {
                text: "Transcribe this audio to text. Return only the transcription, nothing else."
                    .to_string(),
            }],
        },
    };

    let resp = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .context("Failed to send Gemini transcription request")?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse Gemini transcription response")?;

    if !status.is_success() {
        let error_msg = body["error"]["message"]
            .as_str()
            .or_else(|| body["error"]["status"].as_str())
            .unwrap_or("unknown error");
        bail!("Gemini transcription API error ({}): {}", status, error_msg);
    }

    // Extract transcription from Gemini response
    let text = body["candidates"]
        .as_array()
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content["parts"].as_array())
        .and_then(|parts| parts.first())
        .and_then(|part| part["text"].as_str())
        .map(|s| s.to_string())
        .context("Transcription response missing expected fields")?;

    Ok(text)
}

/// Transcribe audio using Groq Whisper API.
async fn transcribe_audio_groq(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
) -> Result<String> {
    use reqwest::multipart::{Form, Part};

    // Normalize extension for Groq
    let normalized_name = match file_name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("oga") => format!("{stem}.ogg"),
        _ => file_name.to_string(),
    };

    let extension = normalized_name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .unwrap_or("");

    let mime = match extension.to_ascii_lowercase().as_str() {
        "flac" => Some("audio/flac"),
        "mp3" | "mpeg" | "mpga" => Some("audio/mpeg"),
        "mp4" | "m4a" => Some("audio/mp4"),
        "ogg" | "oga" => Some("audio/ogg"),
        "opus" => Some("audio/opus"),
        "wav" => Some("audio/wav"),
        "webm" => Some("audio/webm"),
        _ => None,
    }.ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported audio format '.{extension}' — accepted: flac, mp3, mp4, mpeg, mpga, m4a, ogg, opus, wav, webm"
        )
    })?;

    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var("GROQ_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .context(
            "Missing transcription API key: set [transcription].api_key or GROQ_API_KEY environment variable",
        )?;

    let client = crate::config::build_runtime_proxy_client("transcription.groq");

    let file_part = Part::bytes(audio_data)
        .file_name(normalized_name)
        .mime_str(mime)?;

    let mut form = Form::new()
        .part("file", file_part)
        .text("model", config.model.clone())
        .text("response_format", "json");

    if let Some(ref lang) = config.language {
        form = form.text("language", lang.clone());
    }

    let resp = client
        .post(&config.api_url)
        .bearer_auth(&api_key)
        .multipart(form)
        .send()
        .await
        .context("Failed to send transcription request")?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse transcription response")?;

    if !status.is_success() {
        let error_msg = body["error"]["message"].as_str().unwrap_or("unknown error");
        bail!("Transcription API error ({}): {}", status, error_msg);
    }

    let text = body["text"]
        .as_str()
        .context("Transcription response missing 'text' field")?
        .to_string();

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_audio() {
        let big = vec![0u8; MAX_AUDIO_BYTES + 1];
        let config = TranscriptionConfig::default();

        let err = transcribe_audio(big, "test.ogg", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("too large"),
            "expected size error, got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_missing_api_key() {
        // Ensure fallback env keys are absent for this test.
        std::env::remove_var("GROQ_API_KEY");
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("GOOGLE_API_KEY");

        let data = vec![0u8; 100];
        let config = TranscriptionConfig::default();

        let err = transcribe_audio(data, "test.ogg", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("transcription API key"),
            "expected missing-key error, got: {err}"
        );
    }
}
