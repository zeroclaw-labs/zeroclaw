//! WebSocket event schema for the Rust-native voice-chat path.
//!
//! Distinct from `voice/events.rs` (which serves simultaneous
//! interpretation, where the server's job is "translate") because
//! voice-chat semantics are different — the server's job is "answer
//! the user's question" and the message types reflect that:
//!
//!   * No `target_lang` (chat answers in the user's language).
//!   * No `commit_src` / `commit_tgt` distinction (one user turn,
//!     one assistant turn).
//!   * New `assistant_text` event for the LLM reply (separate from
//!     the user transcript so the client can render speaker bubbles).
//!   * Reuses `audio_out` for TTS (same shape as interpretation).
//!
//! ## Protocol
//!
//! ```text
//! Client ──WebSocket──▸ Server
//!     ◂── events ──────────◂
//!
//! 1. C→S: ChatSessionStart    (session_id, source_lang, optional model)
//! 2. S→C: ChatSessionReady    (session ready for audio)
//! 3. C→S: AudioChunk          (PCM16LE 16 kHz mono base64 per chunk)
//! 4.       …repeat 3 until user stops speaking…
//! 5. S→C: UserTranscript      (Gemma ASR result)
//! 6. S→C: ReAsk               (when validation routes to AskUserToRepeat
//!                              or ConfirmInterpretation; SKIP 7-9 for
//!                              this turn — caller waits for next audio
//!                              from the user)
//!         OR proceed to:
//! 7. S→C: AssistantText       (LLM reply text, full)
//! 8. S→C: AudioOut × N        (TTS audio chunks, base64 PCM16LE)
//! 9. S→C: TurnComplete        (this user/assistant turn finished)
//!
//! 10. C→S: ChatSessionStop    (or just close the WebSocket)
//! ```

use serde::{Deserialize, Serialize};

// ── Client → Server ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatClientMessage {
    /// Start a new voice-chat session.
    #[serde(rename = "chat_session_start")]
    SessionStart {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Speaker's language hint (BCP-47, e.g. `"ko"`). Used by the
        /// self-validation pipeline as the script-detection
        /// fallback for short utterances.
        #[serde(rename = "sourceLang", default)]
        source_lang: Option<String>,
        /// LLM provider preference (e.g. `"gemini"`). When absent
        /// the server uses the user's default-provider config.
        #[serde(default)]
        provider: Option<String>,
        /// LLM model preference (e.g. `"gemini-3.1-flash-lite-preview"`).
        /// When absent the server uses the platform default for
        /// `TaskCategory::GeneralChat`.
        #[serde(default)]
        model: Option<String>,
        /// User's voice gender for fallback Typecast voice matching.
        #[serde(rename = "voiceGender", default)]
        voice_gender: Option<String>,
        /// User's voice age band for fallback Typecast voice matching.
        #[serde(rename = "voiceAge", default)]
        voice_age: Option<String>,
        /// Typecast voice clone ID — when present the assistant
        /// replies in the user's cloned voice.
        #[serde(rename = "voiceCloneId", default)]
        voice_clone_id: Option<String>,
        /// Device identifier for logging.
        #[serde(rename = "deviceId", default)]
        device_id: Option<String>,
    },

    /// Stop the current chat session. Equivalent to closing the WS.
    #[serde(rename = "chat_session_stop")]
    SessionStop {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    /// Audio chunk from the user's microphone. PCM16LE, 16 kHz, mono.
    #[serde(rename = "audio_chunk")]
    AudioChunk {
        #[serde(rename = "sessionId")]
        session_id: String,
        seq: u64,
        ts: u64,
        pcm16le: String,
    },

    /// Optional explicit "I am done speaking now" hint. The server
    /// also detects end-of-speech via Gemma ASR's RMS VAD, so this
    /// is a UX-quality hint, not a hard requirement.
    #[serde(rename = "end_of_speech")]
    EndOfSpeech {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

// ── Server → Client ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChatServerMessage {
    /// Session is ready — client may start streaming audio chunks.
    #[serde(rename = "chat_session_ready")]
    SessionReady {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    /// Gemma ASR finalized a user utterance. Sent BEFORE any
    /// validation / LLM step so the client can render the user's
    /// own speech in the chat thread immediately.
    #[serde(rename = "user_transcript")]
    UserTranscript {
        #[serde(rename = "sessionId")]
        session_id: String,
        text: String,
        /// Speaker language detected from the transcript. Same wire
        /// format the validation pipeline uses (`as_str()`).
        #[serde(rename = "detectedLanguage")]
        detected_language: String,
    },

    /// Self-validation routed to a re-ask phase — the server is
    /// asking the user to repeat (route = `ask_user_to_repeat`) or
    /// confirm Gemma's reading (route = `confirm_interpretation`).
    /// The client should display the message in the chat thread
    /// AND play the accompanying audio_out frames; no LLM reply
    /// will follow until the user re-speaks.
    #[serde(rename = "re_ask")]
    ReAsk {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// `"ask_user_to_repeat"` or `"confirm_interpretation"`.
        route: String,
        /// Localized message in the speaker's own language.
        message: String,
        /// What the staircase counter is on the server side (so the
        /// client can show a "1/2 retry" indicator if it wants).
        #[serde(rename = "voiceRetryCount")]
        voice_retry_count: u8,
    },

    /// LLM-generated assistant reply text. Always sent before the
    /// `audio_out` frames for the same turn so the client UI can
    /// render the bubble while TTS is still synthesizing.
    #[serde(rename = "assistant_text")]
    AssistantText {
        #[serde(rename = "sessionId")]
        session_id: String,
        text: String,
    },

    /// TTS audio chunk. Same shape as the interpretation path's
    /// `audio_out` so existing client playback code can be reused.
    #[serde(rename = "audio_out")]
    AudioOut {
        #[serde(rename = "sessionId")]
        session_id: String,
        seq: u64,
        /// Base64-encoded PCM. Sample rate depends on the TTS engine
        /// used (Typecast 44.1 kHz, Kokoro 24 kHz, CosyVoice 24 kHz).
        /// The `sampleRate` field tells the client which to set up.
        #[serde(rename = "sampleRate")]
        sample_rate: u32,
        pcm16le: String,
    },

    /// One user→assistant turn finished. Client may now allow the
    /// next user audio.
    #[serde(rename = "turn_complete")]
    TurnComplete {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    /// Recoverable error — session continues, but this turn failed.
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "sessionId")]
        session_id: String,
        code: String,
        message: String,
    },

    /// Session has ended (server-initiated or after explicit stop).
    #[serde(rename = "chat_session_ended")]
    SessionEnded {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_start_round_trips() {
        let msg = ChatClientMessage::SessionStart {
            session_id: "s1".into(),
            source_lang: Some("ko".into()),
            provider: Some("gemini".into()),
            model: None,
            voice_gender: Some("female".into()),
            voice_age: Some("young_adult".into()),
            voice_clone_id: None,
            device_id: Some("tauri-app".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("chat_session_start"));
        assert!(json.contains("sessionId"));
        assert!(json.contains("voiceGender"));
        let parsed: ChatClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ChatClientMessage::SessionStart {
                session_id,
                source_lang,
                ..
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(source_lang.as_deref(), Some("ko"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn re_ask_serializes_with_camel_case_keys() {
        let msg = ChatServerMessage::ReAsk {
            session_id: "s1".into(),
            route: "ask_user_to_repeat".into(),
            message: "잘 들리지 않습니다…".into(),
            voice_retry_count: 1,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"re_ask\""));
        assert!(json.contains("voiceRetryCount"));
        assert!(json.contains("\"voiceRetryCount\":1"));
    }

    #[test]
    fn assistant_text_round_trips() {
        let msg = ChatServerMessage::AssistantText {
            session_id: "s1".into(),
            text: "Hello!".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ChatServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ChatServerMessage::AssistantText { text, .. } => assert_eq!(text, "Hello!"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn audio_out_carries_sample_rate() {
        let msg = ChatServerMessage::AudioOut {
            session_id: "s1".into(),
            seq: 0,
            sample_rate: 24_000,
            pcm16le: "AA==".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"sampleRate\":24000"));
    }

    #[test]
    fn end_of_speech_round_trips() {
        let msg = ChatClientMessage::EndOfSpeech {
            session_id: "s1".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ChatClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ChatClientMessage::EndOfSpeech { .. }));
    }
}
