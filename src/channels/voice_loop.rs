//! Voice-loop infrastructure for hands-free audio conversations.
//!
//! When voice-loop mode is enabled for a channel, incoming voice messages
//! receive audio responses via TTS synthesis. This module provides the
//! configuration, decision logic, and TTS integration point.
//!
//! The actual TTS synthesis call will be wired when the TTS module lands;
//! until then, [`synthesize_response`] returns `None` and responses fall
//! back to text.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::traits::ChannelMessage;

fn default_voice() -> String {
    "alloy".into()
}

fn default_audio_format() -> String {
    "opus".into()
}

fn default_max_tts_length() -> usize {
    4096
}

/// Voice loop configuration per channel.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct VoiceLoopConfig {
    /// Enable voice-loop mode (auto-reply with audio when voice message received).
    #[serde(default)]
    pub enabled: bool,
    /// Voice to use for TTS.
    #[serde(default = "default_voice")]
    pub voice: String,
    /// Audio format for responses.
    #[serde(default = "default_audio_format")]
    pub audio_format: String,
    /// Maximum character count to synthesize (longer responses sent as text).
    #[serde(default = "default_max_tts_length")]
    pub max_tts_length: usize,
}

impl Default for VoiceLoopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: default_voice(),
            audio_format: default_audio_format(),
            max_tts_length: default_max_tts_length(),
        }
    }
}

/// Determines if a response should be sent as audio based on the original message and config.
pub fn should_reply_as_audio(
    original_message: &ChannelMessage,
    voice_config: &VoiceLoopConfig,
    response_text: &str,
) -> bool {
    voice_config.enabled
        && original_message.is_voice.unwrap_or(false)
        && response_text.chars().count() <= voice_config.max_tts_length
}

/// Placeholder for TTS synthesis — returns `None` until TTS module is integrated.
///
/// This will be wired to `TtsManager` once the TTS PR lands.
pub fn synthesize_response(
    text: &str,
    _config: &VoiceLoopConfig,
) -> anyhow::Result<Option<Vec<u8>>> {
    tracing::debug!(
        text_len = text.len(),
        "Voice loop: TTS synthesis not yet available, falling back to text"
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(is_voice: Option<bool>) -> ChannelMessage {
        ChannelMessage {
            id: "msg_1".into(),
            sender: "test_user".into(),
            reply_target: "test_user".into(),
            content: "hello".into(),
            channel: "test".into(),
            timestamp: 0,
            thread_ts: None,
            is_voice,
        }
    }

    #[test]
    fn defaults_are_correct() {
        let config = VoiceLoopConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.voice, "alloy");
        assert_eq!(config.audio_format, "opus");
        assert_eq!(config.max_tts_length, 4096);
    }

    #[test]
    fn should_reply_when_enabled_and_voice_message() {
        let config = VoiceLoopConfig {
            enabled: true,
            ..Default::default()
        };
        let msg = make_message(Some(true));
        assert!(should_reply_as_audio(&msg, &config, "short reply"));
    }

    #[test]
    fn should_not_reply_when_disabled() {
        let config = VoiceLoopConfig::default();
        let msg = make_message(Some(true));
        assert!(!should_reply_as_audio(&msg, &config, "short reply"));
    }

    #[test]
    fn should_not_reply_when_not_voice_message() {
        let config = VoiceLoopConfig {
            enabled: true,
            ..Default::default()
        };
        let msg = make_message(Some(false));
        assert!(!should_reply_as_audio(&msg, &config, "short reply"));
    }

    #[test]
    fn should_not_reply_when_is_voice_none() {
        let config = VoiceLoopConfig {
            enabled: true,
            ..Default::default()
        };
        let msg = make_message(None);
        assert!(!should_reply_as_audio(&msg, &config, "short reply"));
    }

    #[test]
    fn should_not_reply_when_text_exceeds_max_length() {
        let config = VoiceLoopConfig {
            enabled: true,
            max_tts_length: 10,
            ..Default::default()
        };
        let msg = make_message(Some(true));
        assert!(!should_reply_as_audio(
            &msg,
            &config,
            "this text is longer than ten bytes"
        ));
    }

    #[test]
    fn should_reply_at_exact_max_length_boundary() {
        let config = VoiceLoopConfig {
            enabled: true,
            max_tts_length: 5,
            ..Default::default()
        };
        let msg = make_message(Some(true));
        // Exactly 5 characters — should pass
        assert!(should_reply_as_audio(&msg, &config, "hello"));
        // 6 characters — should fail
        assert!(!should_reply_as_audio(&msg, &config, "hello!"));
    }

    #[test]
    fn synthesize_response_returns_none() {
        let config = VoiceLoopConfig::default();
        let result = synthesize_response("test text", &config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn deserialize_with_defaults() {
        let json = r#"{"enabled": true}"#;
        let config: VoiceLoopConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.voice, "alloy");
        assert_eq!(config.audio_format, "opus");
        assert_eq!(config.max_tts_length, 4096);
    }

    #[test]
    fn deserialize_empty_object_uses_all_defaults() {
        let json = "{}";
        let config: VoiceLoopConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.voice, "alloy");
    }
}
