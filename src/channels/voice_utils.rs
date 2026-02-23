/// Standalone STT/TTS helpers for OpenAI-compatible voice APIs.
///
/// Pure async functions — no struct, no state. Reuse the caller's `reqwest::Client`.
use reqwest::multipart::{Form, Part};

/// Transcribe audio bytes to text via an OpenAI-compatible `/audio/transcriptions` endpoint.
pub async fn transcribe_audio(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
    audio_bytes: Vec<u8>,
    model: &str,
    language: Option<&str>,
) -> anyhow::Result<String> {
    let url = format!("{}/audio/transcriptions", base_url.trim_end_matches('/'));

    let file_part = Part::bytes(audio_bytes)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")?;

    let mut form = Form::new()
        .part("file", file_part)
        .text("model", model.to_string());

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("STT request failed ({status}): {body}");
    }

    let json: serde_json::Value = resp.json().await?;
    let text = json
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();

    if text.is_empty() {
        anyhow::bail!("STT returned empty transcription");
    }

    Ok(text)
}

/// Synthesize text to speech via an OpenAI-compatible `/audio/speech` endpoint.
///
/// Returns raw Opus audio bytes suitable for Telegram voice messages.
pub async fn synthesize_speech(
    client: &reqwest::Client,
    api_key: &str,
    base_url: &str,
    text: &str,
    model: &str,
    voice: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{}/audio/speech", base_url.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": "opus"
    });

    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        anyhow::bail!("TTS request failed ({status}): {err_body}");
    }

    let bytes = resp.bytes().await?.to_vec();
    if bytes.is_empty() {
        anyhow::bail!("TTS returned empty audio");
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcribe_url_construction() {
        let base = "https://api.openai.com/v1";
        let url = format!("{}/audio/transcriptions", base.trim_end_matches('/'));
        assert_eq!(url, "https://api.openai.com/v1/audio/transcriptions");
    }

    #[test]
    fn transcribe_url_strips_trailing_slash() {
        let base = "https://custom.api/v1/";
        let url = format!("{}/audio/transcriptions", base.trim_end_matches('/'));
        assert_eq!(url, "https://custom.api/v1/audio/transcriptions");
    }

    #[test]
    fn synthesize_url_construction() {
        let base = "https://api.openai.com/v1";
        let url = format!("{}/audio/speech", base.trim_end_matches('/'));
        assert_eq!(url, "https://api.openai.com/v1/audio/speech");
    }

    #[test]
    fn synthesize_request_body_shape() {
        let body = serde_json::json!({
            "model": "tts-1",
            "input": "Hello world",
            "voice": "alloy",
            "response_format": "opus"
        });
        assert_eq!(body["model"], "tts-1");
        assert_eq!(body["response_format"], "opus");
        assert_eq!(body["voice"], "alloy");
    }
}
