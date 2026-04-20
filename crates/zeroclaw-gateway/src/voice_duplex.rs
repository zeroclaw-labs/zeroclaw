//! Voice duplex event dispatch for WebSocket sessions.
#![cfg(feature = "gateway-voice-duplex")]

use zeroclaw_api::vad::VoiceEvent;

/// Attempt to parse a text frame as a voice event.
///
/// Returns `Some(VoiceEvent)` if the JSON parses as a known voice event type,
/// or `None` if it's not a voice event (let it fall through to normal handling).
pub fn try_parse_voice_event(text: &str) -> Option<VoiceEvent> {
    serde_json::from_str::<VoiceEvent>(text).ok()
}

/// Handle a parsed voice event.
///
/// For now this is a stub that logs the event. Real behavior
/// (VAD integration, TTS cancellation) will follow in later PRs.
pub fn handle_voice_event(event: VoiceEvent) {
    match event {
        VoiceEvent::SpeechStart => {
            tracing::debug!("voice duplex: speech_start received");
        }
        VoiceEvent::SpeechEnd { transcript } => {
            tracing::debug!(
                transcript = ?transcript,
                "voice duplex: speech_end received"
            );
        }
        VoiceEvent::BargeIn => {
            tracing::debug!("voice duplex: barge_in received");
            // TODO: wire into session abort mechanism (ref upstream PR #5705)
        }
        VoiceEvent::TtsCancel | VoiceEvent::TtsChunk { .. } => {
            // These are server→client events; log a warning if received from client
            tracing::warn!("voice duplex: received server-side event from client");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_speech_start() {
        let event = try_parse_voice_event(r#"{"type":"speech_start"}"#);
        assert!(event.is_some());
    }

    #[test]
    fn parse_speech_end() {
        let event = try_parse_voice_event(r#"{"type":"speech_end","transcript":"hello"}"#);
        assert!(event.is_some());
    }

    #[test]
    fn parse_barge_in() {
        let event = try_parse_voice_event(r#"{"type":"barge_in"}"#);
        assert!(event.is_some());
    }

    #[test]
    fn non_voice_event_returns_none() {
        let event = try_parse_voice_event(r#"{"type":"message","content":"hello"}"#);
        assert!(event.is_none());
    }

    #[test]
    fn invalid_json_returns_none() {
        let event = try_parse_voice_event("not json");
        assert!(event.is_none());
    }

    #[test]
    fn tts_chunk_parse() {
        let event =
            try_parse_voice_event(r#"{"type":"tts_chunk","audio_b64":"AAAA","format":"mp3"}"#);
        assert!(event.is_some());
    }
}
