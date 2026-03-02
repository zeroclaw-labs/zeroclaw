//! Simultaneous interpretation session manager.
//!
//! Ties together:
//! - [`GeminiLiveSession`] for the Gemini Live WebSocket connection
//! - [`SegmentationEngine`] for commit-point segmentation
//! - [`ServerMessage`] events for client communication
//!
//! ## Architecture
//!
//! ```text
//! Client mic ─▸ audio_chunk ─▸ SimulSession ─▸ Gemini Live API
//!                                    │
//!                                    ├─ InputTranscript ─▸ SegmentationEngine
//!                                    │                         │
//!                                    │            commit_src / partial_src
//!                                    │                         │
//!                                    ├─ Audio (translated) ──▸ audio_out ──▸ Client speaker
//!                                    └─ OutputTranscript ────▸ commit_tgt ──▸ Client subtitles
//! ```
//!
//! The session runs as a set of background tasks:
//! 1. **Audio forwarder**: receives client audio, forwards to Gemini Live.
//! 2. **Event processor**: receives Gemini events, runs segmentation, emits client events.
//! 3. **Tick timer**: periodically checks silence-based commit conditions.

use base64::Engine;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::events::{InterpretationMode, ServerMessage};
use super::gemini_live::{GeminiLiveSession, VadConfig, VadSensitivity};
use super::pipeline::{Domain, Formality, InterpreterConfig, LanguageCode, VoiceProviderKind};
use super::simul::{SegmentationConfig, SegmentationEngine};
use super::VoiceEvent;

// ── Session configuration ─────────────────────────────────────────

/// Configuration for a simultaneous interpretation session.
#[derive(Debug, Clone)]
pub struct SimulSessionConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Gemini API key.
    pub api_key: String,
    /// Source language code.
    pub source_lang: LanguageCode,
    /// Target language code.
    pub target_lang: LanguageCode,
    /// Interpretation mode.
    pub mode: InterpretationMode,
    /// Domain specialization.
    pub domain: Domain,
    /// Formality level.
    pub formality: Formality,
    /// Segmentation configuration.
    pub segmentation: SegmentationConfig,
}

// ── Session handle ────────────────────────────────────────────────

/// Handle to a running simultaneous interpretation session.
///
/// Use [`send_audio`] to feed microphone audio, and consume events
/// from [`event_rx`] to get translated audio, subtitles, and status.
pub struct SimulSession {
    /// Channel to send audio to the session.
    audio_tx: mpsc::Sender<Vec<u8>>,
    /// Channel to receive server events (for forwarding to client WebSocket).
    pub event_rx: Arc<Mutex<mpsc::Receiver<ServerMessage>>>,
    /// Session identifier.
    session_id: String,
    /// Signal to stop the session.
    stop_tx: mpsc::Sender<()>,
}

impl SimulSession {
    /// Start a new simultaneous interpretation session.
    ///
    /// Connects to Gemini Live, sets up the segmentation engine, and spawns
    /// background tasks for audio forwarding and event processing.
    pub async fn start(config: SimulSessionConfig) -> anyhow::Result<Self> {
        let session_id = config.session_id.clone();

        // Build interpreter config with simultaneous-optimized system prompt
        let interpreter_config = Self::build_interpreter_config(&config);

        // VAD config: auto mode with low sensitivity for continuous speech.
        // Low sensitivity lets the speaker talk longer without being cut off,
        // which is critical for simultaneous interpretation.
        let vad = VadConfig {
            disabled: false,
            start_sensitivity: VadSensitivity::Low,
            end_sensitivity: super::gemini_live::EndSensitivity::Low,
            prefix_padding_ms: 200,
            silence_duration_ms: 500,
        };

        tracing::info!(
            session_id = %session_id,
            source = config.source_lang.as_str(),
            target = config.target_lang.as_str(),
            mode = ?config.mode,
            "Starting simultaneous interpretation session"
        );

        // Connect to Gemini Live
        let live_session = GeminiLiveSession::connect(
            session_id.clone(),
            &config.api_key,
            &interpreter_config,
            &vad,
        )
        .await?;

        // Channels
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (event_tx, event_rx) = mpsc::channel::<ServerMessage>(256);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);

        let segmentation = Arc::new(Mutex::new(SegmentationEngine::new(config.segmentation)));
        let session_id_clone = session_id.clone();

        // Spawn audio forwarder task
        let live_session_ref = Arc::new(live_session);
        let live_for_audio = Arc::clone(&live_session_ref);
        tokio::spawn(async move {
            Self::audio_forwarder(audio_rx, live_for_audio).await;
        });

        // Spawn event processor task
        let live_for_events = Arc::clone(&live_session_ref);
        let seg_for_events = Arc::clone(&segmentation);
        let event_tx_events = event_tx.clone();
        let sid_events = session_id_clone.clone();
        tokio::spawn(async move {
            Self::event_processor(live_for_events, seg_for_events, event_tx_events, sid_events)
                .await;
        });

        // Spawn tick timer for silence-based commits
        let seg_for_tick = Arc::clone(&segmentation);
        let event_tx_tick = event_tx.clone();
        let sid_tick = session_id_clone.clone();
        tokio::spawn(async move {
            Self::tick_timer(seg_for_tick, event_tx_tick, stop_rx, sid_tick).await;
        });

        // Send session_ready event
        let _ = event_tx
            .send(ServerMessage::SessionReady {
                session_id: session_id.clone(),
                live_session_id: session_id.clone(),
            })
            .await;

        Ok(Self {
            audio_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            session_id,
            stop_tx,
        })
    }

    /// Send a PCM audio chunk to the session (from client microphone).
    pub async fn send_audio(&self, pcm_data: Vec<u8>) -> anyhow::Result<()> {
        self.audio_tx
            .send(pcm_data)
            .await
            .map_err(|_| anyhow::anyhow!("Session audio channel closed"))
    }

    /// Receive the next server event.
    pub async fn recv_event(&self) -> Option<ServerMessage> {
        self.event_rx.lock().await.recv().await
    }

    /// Stop the session gracefully.
    pub async fn stop(&self) {
        let _ = self.stop_tx.send(()).await;
    }

    /// Get the session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    // ── Internal: build config ────────────────────────────────────

    fn build_interpreter_config(config: &SimulSessionConfig) -> InterpreterConfig {
        let bidirectional = config.mode == InterpretationMode::Bidirectional;
        InterpreterConfig {
            source_language: config.source_lang,
            target_language: config.target_lang,
            bidirectional,
            formality: config.formality,
            domain: config.domain,
            preserve_tone: true,
            api_key: None,
            provider: VoiceProviderKind::GeminiLive,
        }
    }

    // ── Internal: audio forwarder ─────────────────────────────────

    /// Receives PCM audio from the client and forwards to Gemini Live.
    async fn audio_forwarder(mut audio_rx: mpsc::Receiver<Vec<u8>>, live: Arc<GeminiLiveSession>) {
        while let Some(pcm) = audio_rx.recv().await {
            if let Err(e) = live.send_audio(&pcm).await {
                tracing::warn!(error = %e, "Failed to forward audio to Gemini Live");
                break;
            }
        }
        tracing::debug!("Audio forwarder stopped");
    }

    // ── Internal: event processor ─────────────────────────────────

    /// Processes events from Gemini Live, runs segmentation on input
    /// transcripts, and emits server messages to the client.
    async fn event_processor(
        live: Arc<GeminiLiveSession>,
        segmentation: Arc<Mutex<SegmentationEngine>>,
        event_tx: mpsc::Sender<ServerMessage>,
        session_id: String,
    ) {
        let mut audio_out_seq: u64 = 0;

        loop {
            let event = match live.recv_event().await {
                Some(e) => e,
                None => break, // Live session ended
            };

            match event {
                VoiceEvent::InputTranscript { text } => {
                    let mut seg = segmentation.lock().await;
                    seg.append_partial(&text);

                    // Send partial source transcript to client
                    let _ = event_tx
                        .send(ServerMessage::PartialSrc {
                            session_id: session_id.clone(),
                            text: seg.partial_text().to_string(),
                            stable_prefix_len: seg.stable_prefix_len(),
                            is_final: false,
                        })
                        .await;

                    // Try to commit segments
                    while let Some(committed) = seg.try_commit() {
                        tracing::info!(
                            session_id = %session_id,
                            commit_id = committed.commit_id,
                            text = %committed.text,
                            "Committed source segment"
                        );
                        let _ = event_tx
                            .send(ServerMessage::CommitSrc {
                                session_id: session_id.clone(),
                                commit_id: committed.commit_id,
                                text: committed.text,
                            })
                            .await;
                    }
                }

                VoiceEvent::OutputTranscript { text } => {
                    // Forward model's translated text as target subtitle
                    let seg = segmentation.lock().await;
                    let latest_commit = seg
                        .committed_segments()
                        .last()
                        .map(|s| s.commit_id)
                        .unwrap_or(0);
                    let _ = event_tx
                        .send(ServerMessage::CommitTgt {
                            session_id: session_id.clone(),
                            commit_id: latest_commit,
                            text,
                        })
                        .await;
                }

                VoiceEvent::Audio { data } => {
                    // Forward translated audio to client for immediate playback
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
                    let _ = event_tx
                        .send(ServerMessage::AudioOut {
                            session_id: session_id.clone(),
                            seq: audio_out_seq,
                            pcm16le: b64,
                        })
                        .await;
                    audio_out_seq += 1;
                }

                VoiceEvent::TurnComplete => {
                    // Flush any remaining stable segments
                    let mut seg = segmentation.lock().await;
                    if let Some(committed) = seg.flush() {
                        let _ = event_tx
                            .send(ServerMessage::CommitSrc {
                                session_id: session_id.clone(),
                                commit_id: committed.commit_id,
                                text: committed.text,
                            })
                            .await;
                    }

                    let _ = event_tx
                        .send(ServerMessage::TurnComplete {
                            session_id: session_id.clone(),
                        })
                        .await;
                }

                VoiceEvent::Interrupted => {
                    let _ = event_tx
                        .send(ServerMessage::Interrupted {
                            session_id: session_id.clone(),
                        })
                        .await;
                }

                VoiceEvent::Error { message } => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %message,
                        "Gemini Live error"
                    );
                    let _ = event_tx
                        .send(ServerMessage::Error {
                            session_id: session_id.clone(),
                            code: "LIVE_API_ERROR".into(),
                            message,
                        })
                        .await;
                }

                VoiceEvent::SetupComplete => {
                    tracing::info!(session_id = %session_id, "Gemini Live setup complete");
                }
            }
        }

        // Session ended — flush all remaining text
        let mut seg = segmentation.lock().await;
        if let Some(committed) = seg.flush_all() {
            let _ = event_tx
                .send(ServerMessage::CommitSrc {
                    session_id: session_id.clone(),
                    commit_id: committed.commit_id,
                    text: committed.text,
                })
                .await;
        }

        let total = seg.committed_segments().len() as u64;
        let _ = event_tx
            .send(ServerMessage::SessionEnded {
                session_id,
                total_segments: total,
            })
            .await;
    }

    // ── Internal: tick timer ──────────────────────────────────────

    /// Periodically checks for silence-based commits.
    async fn tick_timer(
        segmentation: Arc<Mutex<SegmentationEngine>>,
        event_tx: mpsc::Sender<ServerMessage>,
        mut stop_rx: mpsc::Receiver<()>,
        session_id: String,
    ) {
        let tick = tokio::time::Duration::from_millis(100);
        let mut interval = tokio::time::interval(tick);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut seg = segmentation.lock().await;
                    while let Some(committed) = seg.try_commit() {
                        tracing::debug!(
                            session_id = %session_id,
                            commit_id = committed.commit_id,
                            "Silence-based commit"
                        );
                        let _ = event_tx.send(ServerMessage::CommitSrc {
                            session_id: session_id.clone(),
                            commit_id: committed.commit_id,
                            text: committed.text,
                        }).await;
                    }
                }
                _ = stop_rx.recv() => {
                    tracing::debug!(session_id = %session_id, "Tick timer stopped");
                    break;
                }
            }
        }
    }
}
