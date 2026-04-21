//! Voice duplex event dispatch for WebSocket sessions.
#![cfg(feature = "gateway-voice-duplex")]

use serde::{Deserialize, Serialize};

/// Voice event types for the WebSocket duplex protocol.
///
/// These are serialized as JSON text frames. Binary audio frames (PCM16 LE
/// mono 16kHz) are handled separately via [`validate_pcm16_frame`] and
/// [`pcm16_to_f32`].
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Server sends transcription of captured speech.
    #[serde(rename = "transcript")]
    Transcript { text: String },
}

/// Attempt to parse a text frame as a voice event.
///
/// Returns `Some(VoiceEvent)` if the JSON parses as a known voice event type,
/// or `None` if it's not a voice event (let it fall through to normal handling).
pub fn try_parse_voice_event(text: &str) -> Option<VoiceEvent> {
    serde_json::from_str::<VoiceEvent>(text).ok()
}

/// Handle a parsed voice event.
///
/// Returns `None` for successfully handled client→server events.
/// Returns `Some(json)` with an error frame when the client sends
/// a server→client-only event, so the caller can relay it back.
pub fn handle_voice_event(event: VoiceEvent) -> Option<serde_json::Value> {
    match event {
        VoiceEvent::SpeechStart => {
            tracing::debug!("voice duplex: speech_start received");
            None
        }
        VoiceEvent::SpeechEnd { transcript } => {
            tracing::debug!(
                transcript = ?transcript,
                "voice duplex: speech_end received"
            );
            None
        }
        VoiceEvent::BargeIn => {
            tracing::debug!("voice duplex: barge_in received");
            // TODO: wire into session abort mechanism (ref upstream PR #5705)
            None
        }
        VoiceEvent::TtsCancel | VoiceEvent::TtsChunk { .. } => {
            tracing::warn!("voice duplex: received server-side event from client");
            Some(serde_json::json!({
                "type": "error",
                "code": "invalid_event_direction",
                "message": "this event type is server-to-client only"
            }))
        }
        VoiceEvent::Transcript { .. } => {
            tracing::warn!("voice duplex: received server-side transcript event from client");
            Some(serde_json::json!({
                "type": "error",
                "code": "invalid_event_direction",
                "message": "this event type is server-to-client only"
            }))
        }
    }
}

// ── Binary audio frame handling ─────────────────────────────────

/// PCM audio constants for the voice duplex protocol.
///
/// Audio format: **PCM16 little-endian, mono, 16 kHz**.
pub mod audio {
    /// Sample rate in Hz.
    pub const SAMPLE_RATE: u32 = 16_000;
    /// Bytes per sample (16-bit = 2 bytes).
    pub const BYTES_PER_SAMPLE: usize = 2;
    /// Minimum frame duration in milliseconds.
    pub const MIN_FRAME_MS: u32 = 10;
    /// Maximum frame duration in milliseconds.
    pub const MAX_FRAME_MS: u32 = 300;
    /// Minimum frame size in bytes (10 ms × 16 kHz × 2 bytes / 1000 = 320).
    pub const MIN_FRAME_BYTES: usize =
        (SAMPLE_RATE as usize * BYTES_PER_SAMPLE * MIN_FRAME_MS as usize) / 1000;
    /// Maximum frame size in bytes (300 ms × 16 kHz × 2 bytes / 1000 = 9600).
    pub const MAX_FRAME_BYTES: usize =
        (SAMPLE_RATE as usize * BYTES_PER_SAMPLE * MAX_FRAME_MS as usize) / 1000;

    /// Encode f32 samples into a WAV file (PCM16 LE).
    ///
    /// Duplicated from `zeroclaw_channels::voice_wake` to avoid a `cpal`
    /// dependency in the gateway crate. Produces a valid RIFF/WAVE header
    /// followed by 16-bit PCM sample data.
    pub fn encode_wav_from_f32(samples: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
        let bits_per_sample: u16 = 16;
        let byte_rate = u32::from(channels) * sample_rate * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        #[allow(clippy::cast_possible_truncation)]
        let data_len = (samples.len() * 2) as u32;
        let file_len = 36 + data_len;
        let mut buf = Vec::with_capacity(file_len as usize + 8);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&file_len.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&channels.to_le_bytes());
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&byte_rate.to_le_bytes());
        buf.extend_from_slice(&block_align.to_le_bytes());
        buf.extend_from_slice(&bits_per_sample.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        for &sample in samples {
            let clamped = sample.clamp(-1.0, 1.0);
            #[allow(clippy::cast_possible_truncation)]
            let pcm16 = (clamped * 32767.0) as i16;
            buf.extend_from_slice(&pcm16.to_le_bytes());
        }
        buf
    }
}

/// Capability string clients must advertise to enable binary audio frames.
pub const CAP_BINARY_AUDIO: &str = "binary-audio";

/// Errors that can occur when processing binary audio frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioFrameError {
    /// Frame size is not a multiple of 2 (must be complete PCM16 samples).
    InvalidAlignment { bytes: usize },
    /// Frame is too short (below minimum duration).
    TooShort { bytes: usize, min: usize },
    /// Frame is too long (exceeds maximum duration).
    TooLong { bytes: usize, max: usize },
    /// Binary audio not negotiated for this session.
    NotNegotiated,
}

impl std::fmt::Display for AudioFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAlignment { bytes } => write!(
                f,
                "binary frame has odd byte count ({bytes}); PCM16 requires 2-byte alignment"
            ),
            Self::TooShort { bytes, min } => write!(
                f,
                "binary frame too short ({bytes} bytes; minimum {min} = {}ms)",
                audio::MIN_FRAME_MS
            ),
            Self::TooLong { bytes, max } => write!(
                f,
                "binary frame too long ({bytes} bytes; maximum {max} = {}ms)",
                audio::MAX_FRAME_MS
            ),
            Self::NotNegotiated => write!(
                f,
                "binary audio frames not negotiated; send '{CAP_BINARY_AUDIO}' in connect capabilities"
            ),
        }
    }
}

impl std::error::Error for AudioFrameError {}

/// Validate a binary audio frame's size constraints.
///
/// Returns `Ok(())` if the frame is a valid PCM16 buffer within size
/// limits, or an [`AudioFrameError`] describing the problem.
pub fn validate_pcm16_frame(data: &[u8]) -> Result<(), AudioFrameError> {
    let len = data.len();
    if len % audio::BYTES_PER_SAMPLE != 0 {
        return Err(AudioFrameError::InvalidAlignment { bytes: len });
    }
    if len < audio::MIN_FRAME_BYTES {
        return Err(AudioFrameError::TooShort {
            bytes: len,
            min: audio::MIN_FRAME_BYTES,
        });
    }
    if len > audio::MAX_FRAME_BYTES {
        return Err(AudioFrameError::TooLong {
            bytes: len,
            max: audio::MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

/// Convert PCM16 LE samples to f32 samples normalised to [-1.0, 1.0].
///
/// Each pair of bytes is interpreted as a little-endian `i16`, then
/// divided by `i16::MAX` to produce a float in approximately [-1.0, 1.0].
pub fn pcm16_to_f32(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|chunk| {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            sample as f32 / i16::MAX as f32
        })
        .collect()
}

/// Per-session voice duplex state, created when `gateway-voice-duplex` is
/// enabled and the client negotiates binary audio support.
pub struct VoiceDuplexSession {
    /// Whether the client has advertised binary audio capability.
    pub binary_audio: bool,
    /// VAD instance for this session.
    pub vad: Box<dyn zeroclaw_api::vad::Vad>,
    /// Rolling buffer of recent frames kept for pre-speech capture.
    /// Stores the last ~300ms of audio so speech onset isn't lost to VAD latency.
    pre_speech_buffer: std::collections::VecDeque<Vec<f32>>,
    /// Accumulated speech audio between SpeechStart and SpeechEnd.
    speech_buffer: Vec<f32>,
    /// Whether we are currently capturing speech audio.
    is_capturing: bool,
    /// Maximum pre-speech buffer duration in samples (default: 300ms at 16kHz = 4800 samples).
    pre_speech_max_samples: usize,
}

impl VoiceDuplexSession {
    /// Create a new session with binary audio disabled and an energy-based VAD.
    ///
    /// Uses the VAD parameters from `config` to construct an [`EnergyVad`].
    /// Falls back to default threshold (0.01) and silence timeout (500 ms)
    /// if config values are not set.
    pub fn from_config(config: &zeroclaw_config::schema::VoiceDuplexConfig) -> Self {
        let vad: Box<dyn zeroclaw_api::vad::Vad> = Box::new(zeroclaw_api::vad::EnergyVad::new(
            config.vad_energy_threshold,
            u64::from(config.vad_silence_timeout_ms),
        ));
        let pre_speech_max_samples = audio::SAMPLE_RATE as usize * 300 / 1000;
        Self {
            binary_audio: false,
            vad,
            pre_speech_buffer: std::collections::VecDeque::new(),
            speech_buffer: Vec::new(),
            is_capturing: false,
            pre_speech_max_samples,
        }
    }

    /// Create a new session with binary audio disabled and default VAD settings.
    ///
    /// Equivalent to `from_config(&VoiceDuplexConfig::default())`.
    pub fn new() -> Self {
        Self::from_config(&zeroclaw_config::schema::VoiceDuplexConfig::default())
    }

    /// Enable binary audio after successful capability negotiation.
    pub fn enable_binary(&mut self) {
        self.binary_audio = true;
    }

    /// Process a validated PCM16 frame through the VAD pipeline.
    ///
    /// Returns the [`VadEvent`] produced by the underlying VAD implementation.
    /// Also manages pre-speech and speech buffers for audio capture.
    pub fn process_frame(&mut self, f32_samples: &[f32]) -> zeroclaw_api::vad::VadEvent {
        let event = self.vad.process(f32_samples);

        match event {
            zeroclaw_api::vad::VadEvent::SpeechStart => {
                // Copy pre-speech buffer into speech buffer to capture onset
                self.speech_buffer.clear();
                for frame in &self.pre_speech_buffer {
                    self.speech_buffer.extend_from_slice(frame);
                }
                self.speech_buffer.extend_from_slice(f32_samples);
                self.is_capturing = true;
                self.pre_speech_buffer.clear();
            }
            zeroclaw_api::vad::VadEvent::SpeechEnd => {
                if self.is_capturing {
                    self.speech_buffer.extend_from_slice(f32_samples);
                }
                self.is_capturing = false;
            }
            zeroclaw_api::vad::VadEvent::Silence => {
                if self.is_capturing {
                    self.speech_buffer.extend_from_slice(f32_samples);
                } else {
                    // Roll pre-speech buffer
                    self.pre_speech_buffer.push_back(f32_samples.to_vec());
                    let mut total: usize = self.pre_speech_buffer.iter().map(|f| f.len()).sum();
                    while total > self.pre_speech_max_samples && self.pre_speech_buffer.len() > 1 {
                        let removed = self.pre_speech_buffer.pop_front().unwrap();
                        total -= removed.len();
                    }
                }
            }
        }

        event
    }

    /// Drain captured speech audio, resetting the buffer for the next utterance.
    /// Returns the captured f32 samples (including pre-speech buffer).
    pub fn drain_captured_audio(&mut self) -> Vec<f32> {
        self.is_capturing = false;
        std::mem::take(&mut self.speech_buffer)
    }
}

impl Default for VoiceDuplexSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Roundtrip serialization tests (moved from zeroclaw-api) ──

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

    // ── Parse tests ──

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

    // ── Error frame tests ──

    #[test]
    fn server_events_return_error_frame() {
        let cancel_result = handle_voice_event(VoiceEvent::TtsCancel);
        assert!(cancel_result.is_some());
        let err = cancel_result.unwrap();
        assert_eq!(err["type"], "error");
        assert_eq!(err["code"], "invalid_event_direction");

        let chunk_result = handle_voice_event(VoiceEvent::TtsChunk {
            audio_b64: "AAAA".into(),
            format: None,
        });
        assert!(chunk_result.is_some());
        assert_eq!(chunk_result.unwrap()["code"], "invalid_event_direction");
    }

    // ── Binary audio frame tests ──

    #[test]
    fn validate_frame_min_size() {
        // Exactly MIN_FRAME_BYTES should pass
        let frame = vec![0u8; audio::MIN_FRAME_BYTES];
        assert!(validate_pcm16_frame(&frame).is_ok());
    }

    #[test]
    fn validate_frame_max_size() {
        // Exactly MAX_FRAME_BYTES should pass
        let frame = vec![0u8; audio::MAX_FRAME_BYTES];
        assert!(validate_pcm16_frame(&frame).is_ok());
    }

    #[test]
    fn validate_frame_too_short() {
        let frame = vec![0u8; audio::MIN_FRAME_BYTES - 2];
        let err = validate_pcm16_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            AudioFrameError::TooShort {
                bytes: audio::MIN_FRAME_BYTES - 2,
                min: audio::MIN_FRAME_BYTES,
            }
        );
    }

    #[test]
    fn validate_frame_too_long() {
        let frame = vec![0u8; audio::MAX_FRAME_BYTES + 2];
        let err = validate_pcm16_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            AudioFrameError::TooLong {
                bytes: audio::MAX_FRAME_BYTES + 2,
                max: audio::MAX_FRAME_BYTES,
            }
        );
    }

    #[test]
    fn validate_frame_odd_bytes() {
        let frame = vec![0u8; audio::MIN_FRAME_BYTES + 1]; // odd count
        let err = validate_pcm16_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            AudioFrameError::InvalidAlignment {
                bytes: audio::MIN_FRAME_BYTES + 1
            }
        );
    }

    #[test]
    fn pcm16_to_f32_conversion() {
        // Zero samples → 0.0
        let zeros = vec![0u8, 0u8, 0u8, 0u8];
        let f32_samples = pcm16_to_f32(&zeros);
        assert_eq!(f32_samples.len(), 2);
        assert_eq!(f32_samples[0], 0.0);
        assert_eq!(f32_samples[1], 0.0);

        // Max positive i16 (0x7FFF) → ~1.0
        let max_pos = vec![0xFFu8, 0x7Fu8];
        let f32_max = pcm16_to_f32(&max_pos);
        assert!((f32_max[0] - 1.0).abs() < f32::EPSILON);

        // Min negative i16 (0x8000) → -1.0 (after i16::MAX division)
        let max_neg = vec![0x00u8, 0x80u8];
        let f32_min = pcm16_to_f32(&max_neg);
        assert!(f32_min[0] <= -1.0);
    }

    #[test]
    fn voice_duplex_session_defaults() {
        let session = VoiceDuplexSession::default();
        assert!(!session.binary_audio);
    }

    #[test]
    fn voice_duplex_session_enable_binary() {
        let mut session = VoiceDuplexSession::new();
        assert!(!session.binary_audio);
        session.enable_binary();
        assert!(session.binary_audio);
    }

    #[test]
    fn voice_duplex_session_process_frame_detects_speech() {
        let mut session = VoiceDuplexSession::new();
        let loud = vec![0.5f32; 160];
        let quiet = vec![0.001f32; 160];

        // Loud input should trigger SpeechStart
        let event = session.process_frame(&loud);
        assert_eq!(event, zeroclaw_api::vad::VadEvent::SpeechStart);

        // Quiet input after speech — no timeout yet → Silence
        assert_eq!(
            session.process_frame(&quiet),
            zeroclaw_api::vad::VadEvent::Silence
        );
    }

    #[test]
    fn voice_duplex_session_from_config_custom() {
        let config = zeroclaw_config::schema::VoiceDuplexConfig {
            enabled: true,
            vad_energy_threshold: 0.5,
            vad_silence_timeout_ms: 100,
        };
        let mut session = VoiceDuplexSession::from_config(&config);

        // Moderate input below high threshold → Silence
        let moderate = vec![0.1f32; 160];
        assert_eq!(
            session.process_frame(&moderate),
            zeroclaw_api::vad::VadEvent::Silence
        );

        // Very loud input above threshold → SpeechStart
        let very_loud = vec![0.9f32; 160];
        assert_eq!(
            session.process_frame(&very_loud),
            zeroclaw_api::vad::VadEvent::SpeechStart
        );
    }

    #[test]
    fn audio_frame_error_display() {
        let err = AudioFrameError::InvalidAlignment { bytes: 5 };
        assert!(err.to_string().contains("odd byte count"));
        let err = AudioFrameError::TooShort {
            bytes: 10,
            min: 320,
        };
        assert!(err.to_string().contains("too short"));
        let err = AudioFrameError::TooLong {
            bytes: 20000,
            max: 9600,
        };
        assert!(err.to_string().contains("too long"));
        let err = AudioFrameError::NotNegotiated;
        assert!(err.to_string().contains("not negotiated"));
    }

    #[test]
    fn client_events_return_no_error() {
        assert!(handle_voice_event(VoiceEvent::SpeechStart).is_none());
        assert!(handle_voice_event(VoiceEvent::SpeechEnd { transcript: None }).is_none());
        assert!(handle_voice_event(VoiceEvent::BargeIn).is_none());
    }

    // ── WAV encoding tests ──

    #[test]
    fn encode_wav_silence_roundtrip() {
        let samples = vec![0.0f32; 160];
        let wav = audio::encode_wav_from_f32(&samples, 16000, 1);
        // RIFF header
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // Data length: 160 samples × 2 bytes = 320
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len, 320);
        // All samples should be zero
        for i in 0..160 {
            let offset = 44 + i * 2;
            let sample = i16::from_le_bytes([wav[offset], wav[offset + 1]]);
            assert_eq!(sample, 0);
        }
    }

    #[test]
    fn encode_wav_nonzero_samples() {
        let samples = vec![0.5f32, -0.5f32, 1.0f32];
        let wav = audio::encode_wav_from_f32(&samples, 16000, 1);
        // Decode back
        let s0 = i16::from_le_bytes([wav[44], wav[45]]);
        let s1 = i16::from_le_bytes([wav[46], wav[47]]);
        let s2 = i16::from_le_bytes([wav[48], wav[49]]);
        // Positive
        assert!(s0 > 0);
        // Negative
        assert!(s1 < 0);
        // Near max
        assert!(s2 > 32000);
    }

    // ── Speech buffer tests ──

    #[test]
    fn buffer_accumulates_during_speech() {
        let mut session = VoiceDuplexSession::new();
        let loud = vec![0.8f32; 160];
        let quiet = vec![0.001f32; 160];

        // Trigger speech
        let e = session.process_frame(&loud);
        assert_eq!(e, zeroclaw_api::vad::VadEvent::SpeechStart);

        // Feed more loud frames — still capturing
        session.process_frame(&loud);
        session.process_frame(&loud);

        // Should have accumulated: 3 frames × 160 = 480 samples
        assert_eq!(session.speech_buffer.len(), 480);
    }

    #[test]
    fn pre_speech_buffer_captured_on_speech_start() {
        let mut session = VoiceDuplexSession::new();
        let quiet = vec![0.001f32; 160];
        let loud = vec![0.8f32; 160];

        // Feed quiet frames to fill pre-speech buffer
        session.process_frame(&quiet);
        session.process_frame(&quiet);
        session.process_frame(&quiet);

        // Trigger speech — pre-speech buffer should be prepended
        let e = session.process_frame(&loud);
        assert_eq!(e, zeroclaw_api::vad::VadEvent::SpeechStart);

        // 3 quiet frames + 1 loud frame = 4 × 160 = 640 samples
        assert_eq!(session.speech_buffer.len(), 640);
    }

    #[test]
    fn drain_captured_audio_returns_and_resets() {
        let mut session = VoiceDuplexSession::new();
        let loud = vec![0.8f32; 160];

        // Trigger speech and accumulate
        session.process_frame(&loud);
        assert_eq!(session.speech_buffer.len(), 160);

        // Drain should return the samples
        let drained = session.drain_captured_audio();
        assert_eq!(drained.len(), 160);

        // Buffer should be empty after drain
        let drained_again = session.drain_captured_audio();
        assert!(drained_again.is_empty());
    }

    #[test]
    fn empty_buffer_returns_empty_vec() {
        let mut session = VoiceDuplexSession::new();
        let drained = session.drain_captured_audio();
        assert!(drained.is_empty());
    }

    #[test]
    fn pre_speech_buffer_size_bounded() {
        let mut session = VoiceDuplexSession::new();
        let quiet = vec![0.001f32; 160];

        // Feed many quiet frames — should not exceed pre_speech_max_samples
        for _ in 0..50 {
            session.process_frame(&quiet);
        }

        // Check total samples in pre-speech buffer
        let total: usize = session.pre_speech_buffer.iter().map(|f| f.len()).sum();
        assert!(total <= session.pre_speech_max_samples + 160); // allow one frame overshoot
    }

    // ── Transcript event tests ──

    #[test]
    fn transcript_event_serialization_roundtrip() {
        let event = VoiceEvent::Transcript {
            text: "hello world".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"transcript\""));
        assert!(json.contains("\"text\":\"hello world\""));

        let parsed: VoiceEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            VoiceEvent::Transcript { text } => {
                assert_eq!(text, "hello world");
            }
            _ => panic!("expected Transcript"),
        }
    }

    #[test]
    fn transcript_is_server_only() {
        let result = handle_voice_event(VoiceEvent::Transcript {
            text: "test".to_string(),
        });
        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err["type"], "error");
        assert_eq!(err["code"], "invalid_event_direction");
    }
}
