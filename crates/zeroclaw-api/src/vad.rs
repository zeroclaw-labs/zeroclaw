//! Voice Activity Detection trait and event types.

/// Result of processing a chunk of audio samples through a VAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// No speech detected; continue listening.
    Silence,
    /// Speech has just started.
    SpeechStart,
    /// Speech has just ended.
    SpeechEnd,
}

/// Pluggable Voice Activity Detector.
///
/// Implementations receive mono f32 samples and emit [`VadEvent`] transitions.
pub trait Vad: Send + Sync {
    /// Process a buffer of mono f32 samples and return the detected event.
    fn process(&mut self, samples: &[f32]) -> VadEvent;
}

/// No-op VAD that always reports silence.
///
/// Used when `gateway-voice-duplex` is enabled but no real VAD implementation
/// is configured. A real implementation (energy-based or webrtcvad) will
/// follow in a separate PR.
#[derive(Debug, Default)]
pub struct NoopVad;

impl Vad for NoopVad {
    fn process(&mut self, _samples: &[f32]) -> VadEvent {
        VadEvent::Silence
    }
}

/// Voice event types for the WebSocket duplex protocol.
///
/// These are serialized as JSON text frames. Using base64-encoded audio
/// in the `tts_chunk` variant means the existing `Message::Text` path
/// handles everything — no binary frame changes needed yet.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum VoiceEvent {
    /// Client signals that speech has started.
    #[serde(rename = "speech_start")]
    SpeechStart,

    /// Client signals that speech has ended, with optional transcript.
    #[serde(rename = "speech_end")]
    SpeechEnd {
        #[serde(default)]
        transcript: Option<String>,
    },

    /// Client requests cancellation of in-progress TTS.
    #[serde(rename = "barge_in")]
    BargeIn,

    /// Server cancels in-progress TTS.
    #[serde(rename = "tts_cancel")]
    TtsCancel,

    /// Server sends a chunk of base64-encoded audio.
    #[serde(rename = "tts_chunk")]
    TtsChunk {
        audio_b64: String,
        #[serde(default)]
        format: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_vad_always_silence() {
        let mut vad = NoopVad::default();
        assert_eq!(vad.process(&[0.0; 160]), VadEvent::Silence);
        assert_eq!(vad.process(&[0.5; 160]), VadEvent::Silence);
    }

    #[test]
    fn voice_event_speech_start_roundtrip() {
        let event = VoiceEvent::SpeechStart;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, "{\"type\":\"speech_start\"}");
    }

    #[test]
    fn voice_event_speech_end_roundtrip() {
        let json = r#"{"type":"speech_end","transcript":"hello"}"#;
        let event: VoiceEvent = serde_json::from_str(json).unwrap();
        match event {
            VoiceEvent::SpeechEnd { transcript } => {
                assert_eq!(transcript.as_deref(), Some("hello"));
            }
            _ => panic!("expected SpeechEnd"),
        }
    }

    #[test]
    fn voice_event_barge_in_roundtrip() {
        let event = VoiceEvent::BargeIn;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, "{\"type\":\"barge_in\"}");
    }

    #[test]
    fn voice_event_tts_chunk_roundtrip() {
        let event = VoiceEvent::TtsChunk {
            audio_b64: "AAAA".to_string(),
            format: Some("mp3".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: VoiceEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, VoiceEvent::TtsChunk { .. }));
    }
}
