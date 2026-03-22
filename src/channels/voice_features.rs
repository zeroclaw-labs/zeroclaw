//! Channel-specific voice feature descriptors.
//!
//! Declares the voice capabilities available per channel platform and
//! provides helpers for resolving audio attachments to transcribable data.
//!
//! Channels that support voice should embed a [`ChannelVoiceConfig`] and
//! use the parsing helpers to extract voice messages from platform payloads.

use serde::{Deserialize, Serialize};

use crate::config::TranscriptionConfig;

// ── Voice capability descriptor ─────────────────────────────────

/// Describes the voice capabilities of a specific channel platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelVoiceCapabilities {
    /// Channel identifier (e.g. "telegram", "discord", "kakao").
    pub channel: String,
    /// Whether the channel can receive voice messages from users.
    pub receive_voice: bool,
    /// Whether the channel can send voice/audio responses.
    pub send_voice: bool,
    /// Whether the channel supports real-time voice streaming.
    pub realtime_streaming: bool,
    /// Supported audio formats for incoming messages.
    pub supported_formats: Vec<String>,
    /// Maximum voice message duration in seconds (0 = unlimited).
    pub max_duration_secs: u64,
    /// Notes about platform-specific limitations.
    pub notes: String,
}

/// Get the voice capabilities for Telegram.
pub fn telegram_voice_capabilities() -> ChannelVoiceCapabilities {
    ChannelVoiceCapabilities {
        channel: "telegram".into(),
        receive_voice: true,
        send_voice: true,
        realtime_streaming: false,
        supported_formats: vec![
            "ogg".into(),
            "opus".into(),
            "mp3".into(),
            "m4a".into(),
            "wav".into(),
            "flac".into(),
            "webm".into(),
        ],
        max_duration_secs: 0, // No platform limit (config-driven)
        notes: "Voice notes (OGG/Opus) and audio files supported. \
                Transcription via Groq Whisper API."
            .into(),
    }
}

/// Get the voice capabilities for Discord.
pub fn discord_voice_capabilities() -> ChannelVoiceCapabilities {
    ChannelVoiceCapabilities {
        channel: "discord".into(),
        receive_voice: true,
        send_voice: true,
        realtime_streaming: false,
        supported_formats: vec![
            "ogg".into(),
            "mp3".into(),
            "m4a".into(),
            "wav".into(),
            "webm".into(),
            "flac".into(),
        ],
        max_duration_secs: 600, // Discord voice messages up to 10 min
        notes: "Discord voice messages arrive as attachments with \
                content_type 'audio/*'. Download via CDN URL."
            .into(),
    }
}

/// Get the voice capabilities for KakaoTalk.
pub fn kakao_voice_capabilities() -> ChannelVoiceCapabilities {
    ChannelVoiceCapabilities {
        channel: "kakao".into(),
        receive_voice: true,
        send_voice: false, // KakaoTalk Channel API does not support sending audio
        realtime_streaming: false,
        supported_formats: vec!["m4a".into(), "mp3".into(), "aac".into()],
        max_duration_secs: 300, // KakaoTalk voice messages up to 5 min
        notes: "Voice messages via webhook contain an audio URL. \
                Download and transcribe. Sending audio not supported by Channel API."
            .into(),
    }
}

// ── Voice metadata (generic) ────────────────────────────────────

/// Generic voice message metadata extracted from a channel payload.
#[derive(Debug, Clone)]
pub struct VoiceMessageMeta {
    /// Platform-specific file identifier or download URL.
    pub file_ref: String,
    /// Estimated duration in seconds (0 if unknown).
    pub duration_secs: u64,
    /// Original filename hint (may be absent).
    pub file_name: Option<String>,
    /// MIME type hint (may be absent).
    pub mime_type: Option<String>,
    /// Whether this is a voice note (short, recorded inline) vs uploaded file.
    pub is_voice_note: bool,
}

// ── Discord voice parsing ────────────────────────────────────────

/// Attempt to extract voice metadata from a Discord MESSAGE_CREATE attachment.
///
/// Discord voice messages are regular attachments with `content_type` starting
/// with `audio/` and an optional `waveform` field (voice message indicator).
pub fn parse_discord_voice_attachment(attachment: &serde_json::Value) -> Option<VoiceMessageMeta> {
    let content_type = attachment
        .get("content_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !content_type.starts_with("audio/") {
        return None;
    }

    let url = attachment.get("url").and_then(|v| v.as_str())?;
    let filename = attachment
        .get("filename")
        .and_then(|v| v.as_str())
        .map(String::from);
    let duration = attachment
        .get("duration_secs")
        .and_then(|v| v.as_f64())
        .map(|d| d as u64)
        .unwrap_or(0);
    let is_voice_note = attachment.get("waveform").is_some();

    Some(VoiceMessageMeta {
        file_ref: url.to_string(),
        duration_secs: duration,
        file_name: filename,
        mime_type: Some(content_type.to_string()),
        is_voice_note,
    })
}

/// Download audio from a Discord CDN URL.
pub async fn download_discord_audio(
    url: &str,
    bot_token: &str,
) -> anyhow::Result<(Vec<u8>, String)> {
    let client = crate::config::build_runtime_proxy_client("channel.discord");
    let resp = client
        .get(url)
        .header("Authorization", format!("Bot {bot_token}"))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Discord CDN download failed: {}", resp.status());
    }

    let filename = url
        .rsplit('/')
        .next()
        .and_then(|s| s.split('?').next())
        .unwrap_or("voice.ogg")
        .to_string();

    let bytes = resp.bytes().await?.to_vec();
    Ok((bytes, filename))
}

// ── KakaoTalk voice parsing ─────────────────────────────────────

/// Attempt to extract voice metadata from a KakaoTalk webhook payload.
///
/// KakaoTalk sends voice messages in the `extra` field with type "Audio"
/// or via the attachment object with an audio URL.
pub fn parse_kakao_voice_payload(payload: &serde_json::Value) -> Option<VoiceMessageMeta> {
    // KakaoTalk webhook format: { "content": { "type": "audio", "url": "...", "duration": 5 } }
    let content = payload.get("content")?;
    let msg_type = content.get("type").and_then(|v| v.as_str())?;

    if !matches!(msg_type.to_lowercase().as_str(), "audio" | "voice") {
        return None;
    }

    let url = content.get("url").and_then(|v| v.as_str())?;
    let duration = content
        .get("duration")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    Some(VoiceMessageMeta {
        file_ref: url.to_string(),
        duration_secs: duration,
        file_name: Some("kakao_voice.m4a".into()),
        mime_type: Some("audio/mp4".into()),
        is_voice_note: true,
    })
}

/// Download audio from a KakaoTalk audio URL.
pub async fn download_kakao_audio(
    url: &str,
    rest_api_key: &str,
) -> anyhow::Result<(Vec<u8>, String)> {
    let client = crate::config::build_runtime_proxy_client("channel.kakao");
    let resp = client
        .get(url)
        .header("Authorization", format!("KakaoAK {rest_api_key}"))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("KakaoTalk audio download failed: {}", resp.status());
    }

    let bytes = resp.bytes().await?.to_vec();
    Ok((bytes, "kakao_voice.m4a".to_string()))
}

// ── Transcription helper ────────────────────────────────────────

/// Transcribe a voice message using the shared transcription service.
///
/// Wraps [`super::transcription::transcribe_audio`] with duration/size
/// validation and structured logging.
pub async fn transcribe_voice_message(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
    channel: &str,
) -> anyhow::Result<String> {
    tracing::info!(
        channel,
        file_name,
        bytes = audio_data.len(),
        "Transcribing voice message"
    );

    let text = super::transcription::transcribe_audio(audio_data, file_name, config).await?;

    tracing::info!(channel, chars = text.len(), "Voice transcription completed");

    Ok(text)
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_capabilities() {
        let caps = telegram_voice_capabilities();
        assert!(caps.receive_voice);
        assert!(caps.send_voice);
        assert!(caps.supported_formats.contains(&"ogg".to_string()));
    }

    #[test]
    fn discord_capabilities() {
        let caps = discord_voice_capabilities();
        assert!(caps.receive_voice);
        assert!(caps.send_voice);
        assert_eq!(caps.max_duration_secs, 600);
    }

    #[test]
    fn kakao_capabilities() {
        let caps = kakao_voice_capabilities();
        assert!(caps.receive_voice);
        assert!(!caps.send_voice); // KakaoTalk Channel API cannot send audio
        assert_eq!(caps.max_duration_secs, 300);
    }

    #[test]
    fn parse_discord_voice_attachment_audio() {
        let att = serde_json::json!({
            "id": "123",
            "filename": "voice-message.ogg",
            "content_type": "audio/ogg",
            "url": "https://cdn.discordapp.com/attachments/123/456/voice-message.ogg",
            "waveform": "AAAA",
            "duration_secs": 5.2
        });

        let meta = parse_discord_voice_attachment(&att).unwrap();
        assert!(meta.is_voice_note);
        assert_eq!(meta.duration_secs, 5);
        assert_eq!(meta.file_name.as_deref(), Some("voice-message.ogg"));
        assert!(meta.file_ref.contains("cdn.discordapp.com"));
    }

    #[test]
    fn parse_discord_voice_attachment_non_audio_returns_none() {
        let att = serde_json::json!({
            "id": "123",
            "filename": "image.png",
            "content_type": "image/png",
            "url": "https://cdn.discordapp.com/attachments/123/456/image.png"
        });

        assert!(parse_discord_voice_attachment(&att).is_none());
    }

    #[test]
    fn parse_discord_voice_regular_audio_upload() {
        let att = serde_json::json!({
            "id": "789",
            "filename": "song.mp3",
            "content_type": "audio/mpeg",
            "url": "https://cdn.discordapp.com/attachments/123/789/song.mp3"
        });

        let meta = parse_discord_voice_attachment(&att).unwrap();
        assert!(!meta.is_voice_note); // No waveform → regular audio upload
        assert_eq!(meta.mime_type.as_deref(), Some("audio/mpeg"));
    }

    #[test]
    fn parse_kakao_voice_payload_audio() {
        let payload = serde_json::json!({
            "content": {
                "type": "audio",
                "url": "https://k.kakaocdn.net/audio/abc123.m4a",
                "duration": 10
            }
        });

        let meta = parse_kakao_voice_payload(&payload).unwrap();
        assert!(meta.is_voice_note);
        assert_eq!(meta.duration_secs, 10);
        assert!(meta.file_ref.contains("kakaocdn.net"));
    }

    #[test]
    fn parse_kakao_voice_payload_text_returns_none() {
        let payload = serde_json::json!({
            "content": {
                "type": "text",
                "value": "안녕하세요"
            }
        });

        assert!(parse_kakao_voice_payload(&payload).is_none());
    }

    #[test]
    fn parse_kakao_voice_payload_missing_url_returns_none() {
        let payload = serde_json::json!({
            "content": {
                "type": "audio"
            }
        });

        assert!(parse_kakao_voice_payload(&payload).is_none());
    }
}
