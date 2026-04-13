//! Deepgram-based simultaneous interpretation session.
//!
//! Uses Deepgram STT for speech-to-text, then feeds transcripts into
//! the existing segmentation engine. Translation is handled by emitting
//! committed source segments for the upstream relay to process.
//!
//! ## Architecture
//!
//! ```text
//! Client mic ─▸ audio_chunk ─▸ DeepgramSimulSession ─▸ Deepgram STT API
//!                                    │
//!                                    ├─ SttEvent::Partial ─▸ partial_src
//!                                    ├─ SttEvent::Final ───▸ SegmentationEngine
//!                                    │                         │
//!                                    │            commit_src / partial_src
//!                                    │
//!                                    ├─ SpeechStarted ─────▸ client notification
//!                                    └─ UtteranceEnd ──────▸ flush segments
//! ```
//!
//! Unlike `SimulSession` (Gemini-based), this session does NOT produce
//! translated audio or text — it only produces source transcripts with
//! commit-point segmentation. Translation can be layered on top by the
//! gateway handler or a separate LLM call.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::deepgram_stt::{DeepgramConfig, DeepgramSttSession, SttEvent};
use super::events::ServerMessage;
use super::pipeline::LanguageCode;
use super::simul::{SegmentationConfig, SegmentationEngine};

// ── Session configuration ─────────────────────────────────────────

/// Configuration for a Deepgram-based interpretation session.
#[derive(Debug, Clone)]
pub struct DeepgramSimulConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Deepgram API key.
    pub api_key: String,
    /// Source language code (for Deepgram language parameter).
    pub source_lang: LanguageCode,
    /// Deepgram model (e.g. "nova-3").
    pub model: String,
    /// Segmentation configuration.
    pub segmentation: SegmentationConfig,
}

// ── Session handle ────────────────────────────────────────────────

/// Handle to a running Deepgram-based STT session.
///
/// Provides the same `send_audio` / `event_rx` interface as `SimulSession`
/// for drop-in use in `handle_voice_socket`.
pub struct DeepgramSimulSession {
    /// Channel to send audio to the session.
    audio_tx: mpsc::Sender<Vec<u8>>,
    /// Channel to receive server events.
    pub event_rx: Arc<Mutex<mpsc::Receiver<ServerMessage>>>,
    /// Session identifier.
    session_id: String,
    /// Signal to stop the session.
    stop_tx: mpsc::Sender<()>,
}

impl DeepgramSimulSession {
    /// Start a new Deepgram-based STT session.
    pub async fn start(config: DeepgramSimulConfig) -> anyhow::Result<Self> {
        let session_id = config.session_id.clone();

        // Build Deepgram config
        let dg_lang = if config.source_lang == LanguageCode::Ko
            || config.source_lang == LanguageCode::En
        {
            // For common languages, set explicitly for best accuracy
            super::deepgram_stt::language_code_to_deepgram(&config.source_lang).to_string()
        } else {
            // For other languages or auto-detect, use multilingual model
            "multi".to_string()
        };

        let dg_config = DeepgramConfig {
            api_key: config.api_key,
            model: config.model,
            language: dg_lang,
            interim_results: true,
            smart_format: true,
            punctuate: true,
            endpointing_ms: Some(300),
            utterance_end_ms: Some(1000),
            vad_events: true,
            diarize: false,
            encoding: super::deepgram_stt::INPUT_ENCODING.to_string(),
            sample_rate: super::deepgram_stt::INPUT_SAMPLE_RATE,
            channels: 1,
        };

        tracing::info!(
            session_id = %session_id,
            source = config.source_lang.as_str(),
            model = %dg_config.model,
            language = %dg_config.language,
            "Starting Deepgram STT session"
        );

        // Connect to Deepgram
        let dg_session = DeepgramSttSession::connect(session_id.clone(), &dg_config).await?;

        // Channels
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (event_tx, event_rx) = mpsc::channel::<ServerMessage>(256);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);

        let segmentation = Arc::new(Mutex::new(SegmentationEngine::new(config.segmentation)));
        let dg_session = Arc::new(dg_session);

        // Spawn audio forwarder: client audio → Deepgram
        let dg_for_audio = Arc::clone(&dg_session);
        tokio::spawn(async move {
            Self::audio_forwarder(audio_rx, dg_for_audio).await;
        });

        // Spawn event processor: Deepgram SttEvents → ServerMessages
        let dg_for_events = Arc::clone(&dg_session);
        let seg_for_events = Arc::clone(&segmentation);
        let event_tx_events = event_tx.clone();
        let sid_events = session_id.clone();
        tokio::spawn(async move {
            Self::event_processor(dg_for_events, seg_for_events, event_tx_events, sid_events)
                .await;
        });

        // Spawn tick timer for silence-based commits
        let seg_for_tick = Arc::clone(&segmentation);
        let event_tx_tick = event_tx.clone();
        let sid_tick = session_id.clone();
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

    /// Send a PCM audio chunk to the session.
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

    // ── Internal: audio forwarder ─────────────────────────────────

    async fn audio_forwarder(
        mut audio_rx: mpsc::Receiver<Vec<u8>>,
        dg: Arc<DeepgramSttSession>,
    ) {
        while let Some(pcm) = audio_rx.recv().await {
            if let Err(e) = dg.send_audio(pcm).await {
                tracing::warn!(error = %e, "Failed to forward audio to Deepgram");
                break;
            }
        }
        // Finalize any remaining audio when mic stops
        let _ = dg.finalize().await;
        tracing::debug!("Deepgram audio forwarder stopped");
    }

    // ── Internal: event processor ─────────────────────────────────

    async fn event_processor(
        dg: Arc<DeepgramSttSession>,
        segmentation: Arc<Mutex<SegmentationEngine>>,
        event_tx: mpsc::Sender<ServerMessage>,
        session_id: String,
    ) {
        loop {
            let event = match dg.recv_event().await {
                Some(e) => e,
                None => break,
            };

            match event {
                SttEvent::Ready { request_id } => {
                    tracing::info!(
                        session_id = %session_id,
                        request_id = %request_id,
                        "Deepgram STT ready"
                    );
                }

                SttEvent::Partial { text, .. } => {
                    // Show interim transcript to the user immediately
                    let _ = event_tx
                        .send(ServerMessage::PartialSrc {
                            session_id: session_id.clone(),
                            text,
                            stable_prefix_len: 0,
                            is_final: false,
                        })
                        .await;
                }

                SttEvent::Final {
                    text,
                    speech_final,
                    ..
                } => {
                    // Feed final text into segmentation engine
                    let mut seg = segmentation.lock().await;
                    seg.append_partial(&text);

                    // Send partial with stable prefix
                    let _ = event_tx
                        .send(ServerMessage::PartialSrc {
                            session_id: session_id.clone(),
                            text: seg.partial_text().to_string(),
                            stable_prefix_len: seg.stable_prefix_len(),
                            is_final: speech_final,
                        })
                        .await;

                    // Try to commit segments
                    while let Some(committed) = seg.try_commit() {
                        tracing::info!(
                            session_id = %session_id,
                            commit_id = committed.commit_id,
                            text = %committed.text,
                            "Committed source segment (Deepgram)"
                        );
                        let _ = event_tx
                            .send(ServerMessage::CommitSrc {
                                session_id: session_id.clone(),
                                commit_id: committed.commit_id,
                                text: committed.text,
                            })
                            .await;
                    }

                    // If speech_final, flush remaining stable text
                    if speech_final {
                        if let Some(committed) = seg.flush() {
                            let _ = event_tx
                                .send(ServerMessage::CommitSrc {
                                    session_id: session_id.clone(),
                                    commit_id: committed.commit_id,
                                    text: committed.text,
                                })
                                .await;
                        }
                    }
                }

                SttEvent::SpeechStarted { .. } => {
                    // Informational — client can use this for UI feedback
                    // No ServerMessage variant needed; the partial_src flow handles it
                }

                SttEvent::UtteranceEnd { .. } => {
                    // Silence detected after speech — flush all remaining text
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

                    let _ = event_tx
                        .send(ServerMessage::TurnComplete {
                            session_id: session_id.clone(),
                        })
                        .await;
                }

                SttEvent::Error { message } => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %message,
                        "Deepgram STT error"
                    );
                    let _ = event_tx
                        .send(ServerMessage::Error {
                            session_id: session_id.clone(),
                            code: "DEEPGRAM_STT_ERROR".into(),
                            message,
                        })
                        .await;
                }

                SttEvent::Closed => {
                    tracing::info!(session_id = %session_id, "Deepgram STT session closed");
                    break;
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
                            "Silence-based commit (Deepgram)"
                        );
                        let _ = event_tx.send(ServerMessage::CommitSrc {
                            session_id: session_id.clone(),
                            commit_id: committed.commit_id,
                            text: committed.text,
                        }).await;
                    }
                }
                _ = stop_rx.recv() => {
                    tracing::debug!(session_id = %session_id, "Tick timer stopped (Deepgram)");
                    break;
                }
            }
        }
    }
}
