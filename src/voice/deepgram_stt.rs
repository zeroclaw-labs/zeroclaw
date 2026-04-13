//! Deepgram Streaming STT WebSocket client.
//!
//! Connects to `wss://api.deepgram.com/v1/listen` and streams audio for
//! real-time speech-to-text transcription.
//!
//! ## Protocol
//!
//! 1. **Connect** — open WebSocket with query parameters (model, encoding, etc.)
//! 2. **Stream** — send raw audio as Binary frames
//! 3. **Receive** — JSON text frames: Results, Metadata, SpeechStarted, UtteranceEnd
//! 4. **Control** — send JSON: KeepAlive, Finalize, CloseStream
//!
//! ## Deepgram Response Events
//!
//! - `Results` — transcript with `is_final` and `speech_final` flags
//! - `Metadata` — session metadata (request_id, model info)
//! - `SpeechStarted` — VAD detected speech start (requires `vad_events=true`)
//! - `UtteranceEnd` — silence gap after finalized words (requires `utterance_end_ms`)
//!
//! ## Integration
//!
//! Used in two modes:
//! 1. **Chat STT** — mic input → Deepgram → text inserted into chat
//! 2. **Interpreter STT** — mic input → Deepgram → segmentation → LLM translation

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// ── Constants ──────────────────────────────────────────────────────

/// Deepgram Live STT WebSocket endpoint.
const DEEPGRAM_WS_URL: &str = "wss://api.deepgram.com/v1/listen";

/// Default Deepgram model.
const DEFAULT_MODEL: &str = "nova-3";

/// Audio input: PCM16LE, 16kHz, mono (matches our mic capture pipeline).
pub const INPUT_SAMPLE_RATE: u32 = 16000;
pub const INPUT_ENCODING: &str = "linear16";

// ── Configuration ─────────────────────────────────────────────────

/// Configuration for a Deepgram STT session.
#[derive(Debug, Clone)]
pub struct DeepgramConfig {
    /// Deepgram API key.
    pub api_key: String,
    /// Model name (default: "nova-3").
    pub model: String,
    /// BCP-47 language code (e.g. "ko", "en", "multi"). "multi" enables
    /// the multilingual model with automatic language detection.
    pub language: String,
    /// Enable interim (non-final) results for low-latency display.
    pub interim_results: bool,
    /// Enable smart formatting (punctuation, numerals, etc.).
    pub smart_format: bool,
    /// Enable punctuation.
    pub punctuate: bool,
    /// Endpointing threshold in ms (0 = Deepgram default 10ms, false = disabled).
    /// Controls when `speech_final` is set to true.
    pub endpointing_ms: Option<u32>,
    /// Silence duration in ms after last finalized word to trigger UtteranceEnd.
    /// Range: 1000–5000. Requires `interim_results: true`.
    pub utterance_end_ms: Option<u32>,
    /// Enable VAD events (SpeechStarted).
    pub vad_events: bool,
    /// Enable speaker diarization.
    pub diarize: bool,
    /// Audio encoding (default: "linear16").
    pub encoding: String,
    /// Sample rate (default: 16000).
    pub sample_rate: u32,
    /// Number of audio channels (default: 1).
    pub channels: u16,
}

impl Default for DeepgramConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: DEFAULT_MODEL.to_string(),
            language: "multi".to_string(),
            interim_results: true,
            smart_format: true,
            punctuate: true,
            endpointing_ms: Some(300),
            utterance_end_ms: Some(1000),
            vad_events: true,
            diarize: false,
            encoding: INPUT_ENCODING.to_string(),
            sample_rate: INPUT_SAMPLE_RATE,
            channels: 1,
        }
    }
}

impl DeepgramConfig {
    /// Build the WebSocket URL with query parameters.
    fn build_ws_url(&self) -> String {
        let mut params = vec![
            format!("model={}", self.model),
            format!("encoding={}", self.encoding),
            format!("sample_rate={}", self.sample_rate),
            format!("channels={}", self.channels),
        ];

        if self.language != "multi" {
            params.push(format!("language={}", self.language));
        } else {
            // Use the multilingual model — Deepgram auto-detects language
            params.push("language=multi".to_string());
        }

        if self.interim_results {
            params.push("interim_results=true".to_string());
        }
        if self.smart_format {
            params.push("smart_format=true".to_string());
        }
        if self.punctuate {
            params.push("punctuate=true".to_string());
        }
        if self.vad_events {
            params.push("vad_events=true".to_string());
        }
        if self.diarize {
            params.push("diarize=true".to_string());
        }

        match self.endpointing_ms {
            Some(ms) => params.push(format!("endpointing={ms}")),
            None => params.push("endpointing=false".to_string()),
        }

        if let Some(ms) = self.utterance_end_ms {
            params.push(format!("utterance_end_ms={ms}"));
        }

        format!("{DEEPGRAM_WS_URL}?{}", params.join("&"))
    }
}

// ── Deepgram response types ───────────────────────────────────────

/// Top-level Deepgram server message (detected by `type` field).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum DeepgramServerMessage {
    /// Transcription result (interim or final).
    Results(DeepgramResults),
    /// Session metadata.
    Metadata(DeepgramMetadata),
    /// Speech detected (VAD event).
    SpeechStarted(DeepgramSpeechStarted),
    /// Silence gap detected after finalized words.
    UtteranceEnd(DeepgramUtteranceEnd),
}

/// Transcription result from Deepgram.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramResults {
    /// Channel index [channel, total_channels].
    pub channel_index: Option<Vec<u32>>,
    /// Duration of audio processed so far.
    pub duration: Option<f64>,
    /// Start offset in seconds.
    pub start: Option<f64>,
    /// Whether this is a finalized result (maximum accuracy).
    pub is_final: bool,
    /// Whether the model detected an endpoint (end of speech segment).
    #[serde(default)]
    pub speech_final: bool,
    /// Whether this result was produced by a Finalize control message.
    #[serde(default)]
    pub from_finalize: bool,
    /// Transcription channel data.
    pub channel: DeepgramChannel,
    /// Metadata about the request/model.
    pub metadata: Option<DeepgramResultMetadata>,
}

/// Channel-level transcription data.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramChannel {
    /// Ranked alternatives for this channel.
    pub alternatives: Vec<DeepgramAlternative>,
}

/// A single transcription hypothesis.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramAlternative {
    /// The transcript text.
    pub transcript: String,
    /// Confidence score (0.0–1.0).
    pub confidence: f64,
    /// Detected languages.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Word-level timing and confidence.
    #[serde(default)]
    pub words: Vec<DeepgramWord>,
}

/// Word-level transcription detail.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramWord {
    /// The word text.
    pub word: String,
    /// Start time in seconds.
    pub start: f64,
    /// End time in seconds.
    pub end: f64,
    /// Confidence score.
    pub confidence: f64,
    /// Detected language for this word.
    #[serde(default)]
    pub language: Option<String>,
    /// Punctuated version of the word.
    #[serde(default)]
    pub punctuated_word: Option<String>,
    /// Speaker index (when diarize=true).
    #[serde(default)]
    pub speaker: Option<u32>,
}

/// Metadata embedded in a Results message.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramResultMetadata {
    pub request_id: Option<String>,
    pub model_info: Option<DeepgramModelInfo>,
    pub model_uuid: Option<String>,
}

/// Model info embedded in metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramModelInfo {
    pub name: Option<String>,
    pub version: Option<String>,
    pub arch: Option<String>,
}

/// Session metadata message.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramMetadata {
    pub transaction_key: Option<String>,
    pub request_id: Option<String>,
    pub sha256: Option<String>,
    pub created: Option<String>,
    pub duration: Option<f64>,
    pub channels: Option<u32>,
}

/// SpeechStarted VAD event.
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramSpeechStarted {
    /// Channel [index, total].
    pub channel: Option<Vec<u32>>,
    /// Timestamp in seconds when speech was detected.
    pub timestamp: Option<f64>,
}

/// UtteranceEnd event (silence gap after finalized words).
#[derive(Debug, Clone, Deserialize)]
pub struct DeepgramUtteranceEnd {
    /// Channel [index, total].
    pub channel: Option<Vec<u32>>,
    /// Timestamp of last finalized word's end (-1 if forced finalize).
    pub last_word_end: Option<f64>,
}

// ── STT events (emitted to consumers) ─────────────────────────────

/// Events emitted by a Deepgram STT session.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum SttEvent {
    /// Session is connected and ready to receive audio.
    #[serde(rename = "stt_ready")]
    Ready {
        request_id: String,
    },
    /// Interim transcript (still being refined).
    #[serde(rename = "stt_partial")]
    Partial {
        text: String,
        confidence: f64,
        /// Detected language code (e.g. "en", "ko").
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// Final transcript for a segment (is_final=true).
    #[serde(rename = "stt_final")]
    Final {
        text: String,
        confidence: f64,
        /// Whether the endpoint was also detected (speech_final=true).
        speech_final: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Word-level details.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        words: Vec<SttWord>,
    },
    /// Speech started (VAD event).
    #[serde(rename = "stt_speech_started")]
    SpeechStarted {
        timestamp: f64,
    },
    /// Utterance ended (silence gap after speech).
    #[serde(rename = "stt_utterance_end")]
    UtteranceEnd {
        last_word_end: f64,
    },
    /// Error from Deepgram.
    #[serde(rename = "stt_error")]
    Error {
        message: String,
    },
    /// Session closed.
    #[serde(rename = "stt_closed")]
    Closed,
}

/// Simplified word-level detail for consumers.
#[derive(Debug, Clone, Serialize)]
pub struct SttWord {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
}

// ── Outbound control messages ─────────────────────────────────────

/// Control messages sent to Deepgram.
#[derive(Debug)]
enum ControlMessage {
    /// Raw audio bytes to stream.
    Audio(Vec<u8>),
    /// Keep the connection alive without sending audio.
    KeepAlive,
    /// Request finalization of buffered audio.
    Finalize,
    /// Gracefully close the stream.
    CloseStream,
}

// ── Session ───────────────────────────────────────────────────────

/// A handle for interacting with a Deepgram STT session.
///
/// Created by [`DeepgramSttSession::connect`]. Audio is sent via
/// `send_audio`, events are received via `event_rx`.
pub struct DeepgramSttSession {
    /// Channel to send audio/control messages to Deepgram.
    outbound_tx: mpsc::Sender<ControlMessage>,
    /// Channel to receive STT events.
    pub event_rx: Arc<Mutex<mpsc::Receiver<SttEvent>>>,
    /// Session identifier (for logging).
    session_id: String,
    /// KeepAlive task handle.
    keepalive_handle: Option<tokio::task::JoinHandle<()>>,
}

impl DeepgramSttSession {
    /// Connect to Deepgram and start streaming STT.
    pub async fn connect(
        session_id: String,
        config: &DeepgramConfig,
    ) -> anyhow::Result<Self> {
        let ws_url = config.build_ws_url();

        tracing::info!(
            session_id = %session_id,
            model = %config.model,
            language = %config.language,
            url = %ws_url,
            "Connecting to Deepgram STT"
        );

        // Build WebSocket request with auth header
        let mut request = ws_url.into_client_request()?;
        request.headers_mut().insert(
            "Authorization",
            format!("Token {}", config.api_key).parse()?,
        );

        // Connect
        let (ws_stream, _response) =
            tokio_tungstenite::connect_async(request).await?;

        tracing::info!(session_id = %session_id, "Deepgram WebSocket connected");

        let (ws_sink, ws_source) = ws_stream.split();

        // Channels
        let (outbound_tx, outbound_rx) = mpsc::channel::<ControlMessage>(512);
        let (event_tx, event_rx) = mpsc::channel::<SttEvent>(256);

        // Spawn outbound task: forwards audio + control to WebSocket
        let sid_out = session_id.clone();
        tokio::spawn(async move {
            Self::outbound_task(outbound_rx, ws_sink, sid_out).await;
        });

        // Spawn inbound task: parses Deepgram responses → SttEvent
        let sid_in = session_id.clone();
        tokio::spawn(async move {
            Self::inbound_task(ws_source, event_tx, sid_in).await;
        });

        // Spawn KeepAlive task: sends KeepAlive every 8 seconds when idle
        let keepalive_tx = outbound_tx.clone();
        let sid_ka = session_id.clone();
        let keepalive_handle = tokio::spawn(async move {
            Self::keepalive_task(keepalive_tx, sid_ka).await;
        });

        Ok(Self {
            outbound_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            session_id,
            keepalive_handle: Some(keepalive_handle),
        })
    }

    /// Send raw PCM audio to Deepgram (Binary frame).
    pub async fn send_audio(&self, pcm_data: Vec<u8>) -> anyhow::Result<()> {
        self.outbound_tx
            .send(ControlMessage::Audio(pcm_data))
            .await
            .map_err(|_| anyhow::anyhow!("Deepgram outbound channel closed"))
    }

    /// Request finalization of any buffered audio.
    pub async fn finalize(&self) -> anyhow::Result<()> {
        self.outbound_tx
            .send(ControlMessage::Finalize)
            .await
            .map_err(|_| anyhow::anyhow!("Deepgram outbound channel closed"))
    }

    /// Gracefully close the Deepgram stream.
    pub async fn close(&self) {
        let _ = self.outbound_tx.send(ControlMessage::CloseStream).await;
    }

    /// Receive the next STT event.
    pub async fn recv_event(&self) -> Option<SttEvent> {
        self.event_rx.lock().await.recv().await
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    // ── Internal: outbound task ──────────────────────────────────

    async fn outbound_task(
        mut rx: mpsc::Receiver<ControlMessage>,
        mut sink: futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            WsMessage,
        >,
        session_id: String,
    ) {
        while let Some(msg) = rx.recv().await {
            let ws_msg = match msg {
                ControlMessage::Audio(data) => WsMessage::Binary(data.into()),
                ControlMessage::KeepAlive => {
                    WsMessage::Text(r#"{"type":"KeepAlive"}"#.into())
                }
                ControlMessage::Finalize => {
                    WsMessage::Text(r#"{"type":"Finalize"}"#.into())
                }
                ControlMessage::CloseStream => {
                    let _ = sink.send(WsMessage::Text(r#"{"type":"CloseStream"}"#.into())).await;
                    let _ = sink.close().await;
                    tracing::debug!(session_id = %session_id, "Deepgram CloseStream sent");
                    break;
                }
            };

            if let Err(e) = sink.send(ws_msg).await {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to send to Deepgram"
                );
                break;
            }
        }
        tracing::debug!(session_id = %session_id, "Deepgram outbound task ended");
    }

    // ── Internal: inbound task ───────────────────────────────────

    async fn inbound_task(
        mut source: futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
        event_tx: mpsc::Sender<SttEvent>,
        session_id: String,
    ) {
        while let Some(msg) = source.next().await {
            let text = match msg {
                Ok(WsMessage::Text(t)) => t.to_string(),
                Ok(WsMessage::Close(_)) => {
                    tracing::info!(session_id = %session_id, "Deepgram connection closed");
                    break;
                }
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(session_id = %session_id, error = %e, "Deepgram WS error");
                    let _ = event_tx
                        .send(SttEvent::Error {
                            message: format!("WebSocket error: {e}"),
                        })
                        .await;
                    break;
                }
            };

            // Parse the Deepgram server message
            let server_msg: DeepgramServerMessage = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(
                        session_id = %session_id,
                        error = %e,
                        raw = %text,
                        "Failed to parse Deepgram message"
                    );
                    continue;
                }
            };

            let event = match server_msg {
                DeepgramServerMessage::Results(results) => {
                    Self::process_results(&session_id, results)
                }
                DeepgramServerMessage::Metadata(meta) => {
                    tracing::info!(
                        session_id = %session_id,
                        request_id = ?meta.request_id,
                        "Deepgram session metadata received"
                    );
                    Some(SttEvent::Ready {
                        request_id: meta.request_id.unwrap_or_default(),
                    })
                }
                DeepgramServerMessage::SpeechStarted(started) => {
                    let ts = started.timestamp.unwrap_or(0.0);
                    tracing::debug!(session_id = %session_id, timestamp = ts, "Speech started");
                    Some(SttEvent::SpeechStarted { timestamp: ts })
                }
                DeepgramServerMessage::UtteranceEnd(end) => {
                    let last_end = end.last_word_end.unwrap_or(-1.0);
                    if last_end < 0.0 {
                        // last_word_end == -1 means forced finalize; ignore
                        None
                    } else {
                        tracing::debug!(
                            session_id = %session_id,
                            last_word_end = last_end,
                            "Utterance end"
                        );
                        Some(SttEvent::UtteranceEnd {
                            last_word_end: last_end,
                        })
                    }
                }
            };

            if let Some(evt) = event {
                if event_tx.send(evt).await.is_err() {
                    break;
                }
            }
        }

        let _ = event_tx.send(SttEvent::Closed).await;
        tracing::debug!(session_id = %session_id, "Deepgram inbound task ended");
    }

    /// Process a Deepgram Results message into an SttEvent.
    fn process_results(session_id: &str, results: DeepgramResults) -> Option<SttEvent> {
        let alt = results.channel.alternatives.first()?;
        let transcript = alt.transcript.trim();

        // Skip empty transcripts (common for interim results during silence)
        if transcript.is_empty() {
            return None;
        }

        // Detect primary language from words or alternatives
        let language = alt
            .languages
            .first()
            .cloned()
            .or_else(|| {
                alt.words
                    .iter()
                    .find_map(|w| w.language.clone())
            });

        if results.is_final {
            tracing::debug!(
                session_id = %session_id,
                text = %transcript,
                confidence = alt.confidence,
                speech_final = results.speech_final,
                language = ?language,
                "Deepgram final transcript"
            );

            let words = alt
                .words
                .iter()
                .map(|w| SttWord {
                    word: w.punctuated_word.clone().unwrap_or_else(|| w.word.clone()),
                    start: w.start,
                    end: w.end,
                    confidence: w.confidence,
                    speaker: w.speaker,
                })
                .collect();

            Some(SttEvent::Final {
                text: transcript.to_string(),
                confidence: alt.confidence,
                speech_final: results.speech_final,
                language,
                words,
            })
        } else {
            // Interim result — for real-time display
            Some(SttEvent::Partial {
                text: transcript.to_string(),
                confidence: alt.confidence,
                language,
            })
        }
    }

    // ── Internal: keepalive task ─────────────────────────────────

    /// Sends KeepAlive messages every 8 seconds to prevent the connection
    /// from timing out during periods of silence.
    async fn keepalive_task(tx: mpsc::Sender<ControlMessage>, session_id: String) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(8));
        loop {
            interval.tick().await;
            if tx.send(ControlMessage::KeepAlive).await.is_err() {
                tracing::debug!(session_id = %session_id, "KeepAlive task: channel closed");
                break;
            }
        }
    }
}

impl Drop for DeepgramSttSession {
    fn drop(&mut self) {
        if let Some(handle) = self.keepalive_handle.take() {
            handle.abort();
        }
    }
}

// ── Language code mapping ─────────────────────────────────────────

/// Map our internal LanguageCode to Deepgram's BCP-47 language code.
///
/// Deepgram supports a wide range of languages. For `auto` or `multi`,
/// use `"multi"` which enables automatic language detection with the
/// multilingual nova-3 model.
pub fn language_code_to_deepgram(code: &super::pipeline::LanguageCode) -> &'static str {
    use super::pipeline::LanguageCode;
    match code {
        LanguageCode::Ko => "ko",
        LanguageCode::Ja => "ja",
        LanguageCode::Zh => "zh",
        LanguageCode::ZhTw => "zh-TW",
        LanguageCode::Th => "th",
        LanguageCode::Vi => "vi",
        LanguageCode::Id => "id",
        LanguageCode::Ms => "ms",
        LanguageCode::Tl => "tl",
        LanguageCode::Hi => "hi",
        LanguageCode::En => "en",
        LanguageCode::Es => "es",
        LanguageCode::Fr => "fr",
        LanguageCode::De => "de",
        LanguageCode::It => "it",
        LanguageCode::Pt => "pt",
        LanguageCode::Nl => "nl",
        LanguageCode::Pl => "pl",
        LanguageCode::Cs => "cs",
        LanguageCode::Sv => "sv",
        LanguageCode::Da => "da",
        LanguageCode::Ru => "ru",
        LanguageCode::Uk => "uk",
        LanguageCode::Tr => "tr",
        LanguageCode::Ar => "ar",
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_builds_valid_url() {
        let mut config = DeepgramConfig::default();
        config.api_key = "test-key".into();
        let url = config.build_ws_url();

        assert!(url.starts_with("wss://api.deepgram.com/v1/listen?"));
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("channels=1"));
        assert!(url.contains("language=multi"));
        assert!(url.contains("interim_results=true"));
        assert!(url.contains("smart_format=true"));
        assert!(url.contains("punctuate=true"));
        assert!(url.contains("vad_events=true"));
        assert!(url.contains("endpointing=300"));
        assert!(url.contains("utterance_end_ms=1000"));
    }

    #[test]
    fn config_specific_language_url() {
        let config = DeepgramConfig {
            api_key: "key".into(),
            language: "ko".into(),
            endpointing_ms: Some(500),
            utterance_end_ms: Some(2000),
            diarize: true,
            ..Default::default()
        };
        let url = config.build_ws_url();

        assert!(url.contains("language=ko"));
        assert!(url.contains("endpointing=500"));
        assert!(url.contains("utterance_end_ms=2000"));
        assert!(url.contains("diarize=true"));
    }

    #[test]
    fn config_endpointing_disabled() {
        let config = DeepgramConfig {
            api_key: "key".into(),
            endpointing_ms: None,
            ..Default::default()
        };
        let url = config.build_ws_url();
        assert!(url.contains("endpointing=false"));
    }

    #[test]
    fn parse_results_final() {
        let json = r#"{
            "type": "Results",
            "channel_index": [0, 1],
            "duration": 2.34,
            "start": 0.0,
            "is_final": true,
            "speech_final": true,
            "from_finalize": false,
            "channel": {
                "alternatives": [{
                    "transcript": "안녕하세요",
                    "confidence": 0.95,
                    "languages": ["ko"],
                    "words": [{
                        "word": "안녕하세요",
                        "start": 0.1,
                        "end": 0.8,
                        "confidence": 0.95,
                        "language": "ko",
                        "punctuated_word": "안녕하세요"
                    }]
                }]
            }
        }"#;

        let msg: DeepgramServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            DeepgramServerMessage::Results(r) => {
                assert!(r.is_final);
                assert!(r.speech_final);
                assert_eq!(
                    r.channel.alternatives[0].transcript,
                    "안녕하세요"
                );
                assert_eq!(r.channel.alternatives[0].confidence, 0.95);
            }
            _ => panic!("Expected Results"),
        }
    }

    #[test]
    fn parse_results_interim() {
        let json = r#"{
            "type": "Results",
            "channel_index": [0, 1],
            "duration": 1.0,
            "start": 0.0,
            "is_final": false,
            "speech_final": false,
            "channel": {
                "alternatives": [{
                    "transcript": "hello",
                    "confidence": 0.8,
                    "words": []
                }]
            }
        }"#;

        let msg: DeepgramServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            DeepgramServerMessage::Results(r) => {
                assert!(!r.is_final);
                assert!(!r.speech_final);
            }
            _ => panic!("Expected Results"),
        }
    }

    #[test]
    fn parse_metadata() {
        let json = r#"{
            "type": "Metadata",
            "transaction_key": "abc123",
            "request_id": "550e8400-e29b-41d4-a716-446655440000",
            "sha256": "deadbeef",
            "created": "2026-04-13T00:00:00Z",
            "duration": 0.0,
            "channels": 1
        }"#;

        let msg: DeepgramServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            DeepgramServerMessage::Metadata(m) => {
                assert_eq!(
                    m.request_id.unwrap(),
                    "550e8400-e29b-41d4-a716-446655440000"
                );
                assert_eq!(m.channels, Some(1));
            }
            _ => panic!("Expected Metadata"),
        }
    }

    #[test]
    fn parse_speech_started() {
        let json = r#"{
            "type": "SpeechStarted",
            "channel": [0, 1],
            "timestamp": 1.5
        }"#;

        let msg: DeepgramServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            DeepgramServerMessage::SpeechStarted(s) => {
                assert_eq!(s.timestamp, Some(1.5));
                assert_eq!(s.channel, Some(vec![0, 1]));
            }
            _ => panic!("Expected SpeechStarted"),
        }
    }

    #[test]
    fn parse_utterance_end() {
        let json = r#"{
            "type": "UtteranceEnd",
            "channel": [0, 1],
            "last_word_end": 2.395
        }"#;

        let msg: DeepgramServerMessage = serde_json::from_str(json).unwrap();
        match msg {
            DeepgramServerMessage::UtteranceEnd(u) => {
                assert_eq!(u.last_word_end, Some(2.395));
            }
            _ => panic!("Expected UtteranceEnd"),
        }
    }

    #[test]
    fn process_results_empty_transcript_returns_none() {
        let results = DeepgramResults {
            channel_index: None,
            duration: None,
            start: None,
            is_final: false,
            speech_final: false,
            from_finalize: false,
            channel: DeepgramChannel {
                alternatives: vec![DeepgramAlternative {
                    transcript: "  ".into(),
                    confidence: 0.0,
                    languages: vec![],
                    words: vec![],
                }],
            },
            metadata: None,
        };

        assert!(DeepgramSttSession::process_results("test", results).is_none());
    }

    #[test]
    fn process_results_final_event() {
        let results = DeepgramResults {
            channel_index: None,
            duration: None,
            start: None,
            is_final: true,
            speech_final: true,
            from_finalize: false,
            channel: DeepgramChannel {
                alternatives: vec![DeepgramAlternative {
                    transcript: "Hello world".into(),
                    confidence: 0.99,
                    languages: vec!["en".into()],
                    words: vec![
                        DeepgramWord {
                            word: "hello".into(),
                            start: 0.1,
                            end: 0.4,
                            confidence: 0.99,
                            language: Some("en".into()),
                            punctuated_word: Some("Hello".into()),
                            speaker: None,
                        },
                        DeepgramWord {
                            word: "world".into(),
                            start: 0.5,
                            end: 0.9,
                            confidence: 0.98,
                            language: Some("en".into()),
                            punctuated_word: Some("world".into()),
                            speaker: None,
                        },
                    ],
                }],
            },
            metadata: None,
        };

        let event = DeepgramSttSession::process_results("test", results).unwrap();
        match event {
            SttEvent::Final {
                text,
                confidence,
                speech_final,
                language,
                words,
            } => {
                assert_eq!(text, "Hello world");
                assert_eq!(confidence, 0.99);
                assert!(speech_final);
                assert_eq!(language, Some("en".into()));
                assert_eq!(words.len(), 2);
                assert_eq!(words[0].word, "Hello");
                assert_eq!(words[1].word, "world");
            }
            _ => panic!("Expected Final event"),
        }
    }

    #[test]
    fn language_code_mapping() {
        use super::super::pipeline::LanguageCode;
        assert_eq!(language_code_to_deepgram(&LanguageCode::Ko), "ko");
        assert_eq!(language_code_to_deepgram(&LanguageCode::En), "en");
        assert_eq!(language_code_to_deepgram(&LanguageCode::ZhTw), "zh-TW");
        assert_eq!(language_code_to_deepgram(&LanguageCode::Ja), "ja");
    }

    #[test]
    fn stt_event_serialization() {
        let evt = SttEvent::Final {
            text: "Hello".into(),
            confidence: 0.95,
            speech_final: true,
            language: Some("en".into()),
            words: vec![],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("stt_final"));
        assert!(json.contains("speech_final"));

        let evt2 = SttEvent::Partial {
            text: "Hel".into(),
            confidence: 0.7,
            language: None,
        };
        let json2 = serde_json::to_string(&evt2).unwrap();
        assert!(json2.contains("stt_partial"));
        assert!(!json2.contains("language")); // skipped when None
    }
}
