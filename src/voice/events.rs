//! WebSocket event schema for simultaneous interpretation.
//!
//! Defines the JSON message types exchanged between the MoA client
//! (web / Tauri) and the MoA server during a live interpretation session.
//!
//! ## Protocol
//!
//! ```text
//! MoA Client ──WebSocket──▸ MoA Server ──WebSocket──▸ Gemini Live API
//!     ◂── events ──────────────◂── audio/text ──────────◂
//! ```
//!
//! All messages are JSON text frames. Audio payloads use base64 encoding
//! within JSON (upgrade to binary frames is a future optimization).

use serde::{Deserialize, Serialize};

// ── Client → Server messages ──────────────────────────────────────

/// Messages sent from the MoA client to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Start a new simultaneous interpretation session.
    #[serde(rename = "session_start")]
    SessionStart {
        /// Unique session identifier (UUID).
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Source language code (ISO 639-1, e.g. "ko", "en").
        #[serde(rename = "sourceLang")]
        source_lang: String,
        /// Target language code.
        #[serde(rename = "targetLang")]
        target_lang: String,
        /// Interpretation mode.
        mode: InterpretationMode,
        /// Device identifier (for multi-device speaker separation).
        #[serde(rename = "deviceId")]
        device_id: String,
        /// Voice/STT provider: "gemini", "openai", or "deepgram".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Optional domain specialization.
        #[serde(skip_serializing_if = "Option::is_none")]
        domain: Option<String>,
        /// Optional formality level.
        #[serde(skip_serializing_if = "Option::is_none")]
        formality: Option<String>,
    },

    /// Stop the current session.
    #[serde(rename = "session_stop")]
    SessionStop {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    /// Audio chunk from the client microphone.
    #[serde(rename = "audio_chunk")]
    AudioChunk {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Sequence number for ordering.
        seq: u64,
        /// Client timestamp (ms since epoch).
        ts: u64,
        /// Base64-encoded PCM16LE audio data (16kHz mono).
        pcm16le: String,
    },

    /// Signal that the user pressed/released the talk button (manual VAD).
    #[serde(rename = "activity_signal")]
    ActivitySignal {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// true = speech started, false = speech ended.
        active: bool,
    },
}

// ── Server → Client messages ──────────────────────────────────────

/// Messages sent from the MoA server to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Session is ready for audio streaming.
    #[serde(rename = "session_ready")]
    SessionReady {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Gemini Live session identifier (for debugging).
        #[serde(rename = "liveSessionId")]
        live_session_id: String,
    },

    /// Partial source-language transcript (continuously updated).
    #[serde(rename = "partial_src")]
    PartialSrc {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Full partial transcript so far.
        text: String,
        /// Byte length of the stable (unlikely to change) prefix.
        #[serde(rename = "stablePrefixLen")]
        stable_prefix_len: usize,
        /// Whether this is the final partial for the current utterance.
        #[serde(rename = "final")]
        is_final: bool,
    },

    /// A source-language segment has been committed for translation.
    #[serde(rename = "commit_src")]
    CommitSrc {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "commitId")]
        commit_id: u64,
        /// The committed source text.
        text: String,
    },

    /// Partial target-language transcript (translation in progress).
    #[serde(rename = "partial_tgt")]
    PartialTgt {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "commitId")]
        commit_id: u64,
        /// Partial translated text.
        text: String,
        #[serde(rename = "final")]
        is_final: bool,
    },

    /// Final committed translation for a segment.
    #[serde(rename = "commit_tgt")]
    CommitTgt {
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "commitId")]
        commit_id: u64,
        /// Final translated text.
        text: String,
    },

    /// Audio output chunk (translated speech).
    #[serde(rename = "audio_out")]
    AudioOut {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Sequence number for playback ordering.
        seq: u64,
        /// Base64-encoded PCM16LE audio data (24kHz mono).
        pcm16le: String,
    },

    /// The model's response turn is complete.
    #[serde(rename = "turn_complete")]
    TurnComplete {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    /// The model was interrupted (user started speaking during output).
    #[serde(rename = "interrupted")]
    Interrupted {
        #[serde(rename = "sessionId")]
        session_id: String,
    },

    /// Error occurred.
    #[serde(rename = "error")]
    Error {
        #[serde(rename = "sessionId")]
        session_id: String,
        code: String,
        message: String,
    },

    /// Session has ended.
    #[serde(rename = "session_ended")]
    SessionEnded {
        #[serde(rename = "sessionId")]
        session_id: String,
        /// Total segments committed during the session.
        #[serde(rename = "totalSegments")]
        total_segments: u64,
    },
}

// ── Interpretation mode ───────────────────────────────────────────

/// Mode of interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InterpretationMode {
    /// Simultaneous: translate while speaker is still talking.
    #[default]
    #[serde(rename = "simul")]
    Simultaneous,
    /// Consecutive: wait for speaker to finish, then translate.
    #[serde(rename = "consecutive")]
    Consecutive,
    /// Bidirectional: auto-detect language and interpret both ways.
    #[serde(rename = "bidirectional")]
    Bidirectional,
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_serialization() {
        let msg = ClientMessage::SessionStart {
            session_id: "test-123".into(),
            source_lang: "ko".into(),
            target_lang: "en".into(),
            mode: InterpretationMode::Simultaneous,
            device_id: "device-a".into(),
            provider: None,
            domain: None,
            formality: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("session_start"));
        assert!(json.contains("sessionId"));

        // Round-trip
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::SessionStart { session_id, .. } => {
                assert_eq!(session_id, "test-123");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_message_serialization() {
        let msg = ServerMessage::PartialSrc {
            session_id: "s1".into(),
            text: "Hello world".into(),
            stable_prefix_len: 5,
            is_final: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("partial_src"));
        assert!(json.contains("stablePrefixLen"));
    }

    #[test]
    fn audio_chunk_message() {
        let msg = ClientMessage::AudioChunk {
            session_id: "s1".into(),
            seq: 42,
            ts: 1_700_000_000_000,
            pcm16le: "AAAA".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("audio_chunk"));
        assert!(json.contains("pcm16le"));
    }

    #[test]
    fn interpretation_modes() {
        assert_eq!(
            serde_json::to_string(&InterpretationMode::Simultaneous).unwrap(),
            "\"simul\""
        );
        assert_eq!(
            serde_json::to_string(&InterpretationMode::Bidirectional).unwrap(),
            "\"bidirectional\""
        );
    }
}
