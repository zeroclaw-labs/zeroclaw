//! Voice processing pipeline for ZeroClaw.
//!
//! Provides real-time voice interpretation and voice-to-voice conversation
//! capabilities using Gemini Live Native Audio.
//!
//! ## Design
//! - Trait-driven voice provider abstraction (`VoiceProvider`)
//! - 25-language support with Unicode-based language detection
//! - Bidirectional interpretation mode (A <-> B language auto-switch)
//! - Formality levels (formal / neutral / casual)
//! - Domain specialization (general / business / medical / legal / technical)
//! - Per-session billing integration (token-equivalent credit deduction)
//! - Gemini Live WebSocket client with automatic VAD for hands-free interpretation
//!
//! ## Simultaneous Interpretation
//! - `simul` — commit-point segmentation engine for phrase-level translation
//! - `events` — WebSocket event schema for client-server communication
//! - `simul_session` — session manager tying Live API + segmentation + events

pub mod conference;
pub mod deepgram_simul;
pub mod deepgram_stt;
pub mod events;
pub mod gemini_live;
pub mod openai_realtime;
pub mod pipeline;
pub mod simul;
pub mod simul_session;

// ── Shared voice event type ──────────────────────────────────────

/// Provider-agnostic event produced by any voice session.
///
/// Voice sessions emit these events through their `event_rx` channels,
/// enabling the gateway to relay them to the browser/client.
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// Provider setup completed — ready to stream.
    SetupComplete,
    /// Translated/interpreted audio chunk (PCM16, 24kHz mono).
    Audio { data: Vec<u8> },
    /// Transcription of user's speech (input).
    InputTranscript { text: String },
    /// Transcription of model's speech (output / translated).
    OutputTranscript { text: String },
    /// Model finished a response turn.
    TurnComplete,
    /// The model was interrupted (user started speaking mid-response).
    Interrupted,
    /// Error from the provider.
    Error { message: String },
}

#[allow(unused_imports)]
pub use conference::{ConferenceConfig, ConferenceManager, ConferenceRoom, ConferenceRoomSummary};
#[allow(unused_imports)]
pub use events::{ClientMessage, InterpretationMode, ServerMessage};
#[allow(unused_imports)]
pub use gemini_live::{ConnectionState, GeminiLiveSession, VadConfig, VadSensitivity};
#[allow(unused_imports)]
pub use deepgram_simul::{DeepgramSimulConfig, DeepgramSimulSession};
#[allow(unused_imports)]
pub use deepgram_stt::{DeepgramConfig, DeepgramSttSession, SttEvent};
#[allow(unused_imports)]
pub use openai_realtime::OpenAiRealtimeSession;
#[allow(unused_imports)]
pub use pipeline::{
    Domain, Formality, InterpreterConfig, InterpreterSession, InterpreterStats, InterpreterStatus,
    LanguageCode, VoiceProvider, VoiceProviderKind, VoiceSessionManager,
};
#[allow(unused_imports)]
pub use simul::{CommittedSegment, SegmentationConfig, SegmentationEngine};
#[allow(unused_imports)]
pub use simul_session::{SimulSession, SimulSessionConfig};
