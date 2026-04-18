//! Drop-in replacement for `DeepgramSimulSession` backed by Gemma 4 STT
//! (see [`super::gemma_asr`]).
//!
//! Same `audio_tx` / `event_rx` / `stop` interface so `handle_voice_socket`
//! can swap providers via a `match` on the configured engine without
//! touching the rest of the gateway.
//!
//! Conceptual difference from Deepgram: Gemma already utterance-segments
//! internally via VAD, so each [`SttEvent::Final`] from
//! [`GemmaAsrSession`](super::gemma_asr::GemmaAsrSession) corresponds to
//! one complete commit. No client-side segmentation engine needed — this
//! wrapper just translates events 1:1 into [`ServerMessage`] and
//! synthesises a monotonic `commit_id`.

// Each SttEvent variant gets its own arm even when the body is currently
// empty so future Gemma extensions (Partial, UtteranceEnd) have an obvious
// place to land without unrelated routing churn.
#![allow(clippy::match_same_arms)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::deepgram_stt::SttEvent;
use super::events::ServerMessage;
use super::gemma_asr::{GemmaAsrConfig, GemmaAsrSession};
use super::pipeline::LanguageCode;

/// Configuration for a Gemma 4 simul session.
#[derive(Debug, Clone)]
pub struct GemmaSimulConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Source language code; passed through as a hint to Gemma's prompt.
    pub source_lang: LanguageCode,
    /// Ollama base URL.
    pub base_url: String,
    /// Ollama tag (must be E2B or E4B for audio support).
    pub model: String,
}

/// Session handle compatible with `DeepgramSimulSession` / `SimulSession`.
pub struct GemmaSimulSession {
    audio_tx: mpsc::Sender<Vec<u8>>,
    pub event_rx: Arc<Mutex<mpsc::Receiver<ServerMessage>>>,
    session_id: String,
    stop_tx: mpsc::Sender<()>,
}

impl GemmaSimulSession {
    pub async fn start(config: GemmaSimulConfig) -> anyhow::Result<Self> {
        let session_id = config.session_id.clone();

        let asr_config = GemmaAsrConfig {
            session_id: session_id.clone(),
            base_url: config.base_url,
            model: config.model,
            language_hint: Some(config.source_lang.as_str().to_string()),
            ..Default::default()
        };

        let asr = Arc::new(GemmaAsrSession::start(asr_config).await?);

        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (event_tx, event_rx) = mpsc::channel::<ServerMessage>(64);
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

        // Initial session_ready event so the gateway transitions out of
        // "connecting" exactly like the Deepgram path.
        let _ = event_tx
            .send(ServerMessage::SessionReady {
                session_id: session_id.clone(),
                live_session_id: session_id.clone(),
            })
            .await;

        // Audio forwarder: client → ASR.
        let asr_for_audio = Arc::clone(&asr);
        tokio::spawn(async move {
            while let Some(pcm) = audio_rx.recv().await {
                if asr_for_audio.send_audio(pcm).await.is_err() {
                    break;
                }
            }
        });

        // Event translator: SttEvent → ServerMessage.
        let asr_for_events = Arc::clone(&asr);
        let event_tx_for_events = event_tx.clone();
        let sid = session_id.clone();
        let commit_counter = Arc::new(AtomicU64::new(1));
        let counter_for_events = Arc::clone(&commit_counter);
        let translator_handle = tokio::spawn(async move {
            let mut rx = asr_for_events.event_rx.lock().await;
            while let Some(evt) = rx.recv().await {
                match evt {
                    SttEvent::Ready { request_id } => {
                        tracing::info!(
                            session_id = %sid,
                            request_id = %request_id,
                            "Gemma STT ready"
                        );
                    }
                    SttEvent::SpeechStarted { .. } => {
                        // Gateway / Deepgram path is silent here too.
                    }
                    SttEvent::Final { text, .. } => {
                        let commit_id = counter_for_events.fetch_add(1, Ordering::Relaxed);
                        // 1) interim view of the full utterance
                        let _ = event_tx_for_events
                            .send(ServerMessage::PartialSrc {
                                session_id: sid.clone(),
                                text: text.clone(),
                                stable_prefix_len: text.len(),
                                is_final: true,
                            })
                            .await;
                        // 2) commit
                        let _ = event_tx_for_events
                            .send(ServerMessage::CommitSrc {
                                session_id: sid.clone(),
                                commit_id,
                                text,
                            })
                            .await;
                        // 3) turn complete (one Final == one turn for Gemma)
                        let _ = event_tx_for_events
                            .send(ServerMessage::TurnComplete {
                                session_id: sid.clone(),
                            })
                            .await;
                    }
                    SttEvent::Partial { .. } => {
                        // Gemma never emits Partial today. If a future
                        // streaming variant appears, route it like
                        // Deepgram's PartialSrc.
                    }
                    SttEvent::UtteranceEnd { .. } => {
                        // Already covered by the post-Final TurnComplete.
                    }
                    SttEvent::Error { message } => {
                        let _ = event_tx_for_events
                            .send(ServerMessage::Error {
                                session_id: sid.clone(),
                                code: "GEMMA_STT_ERROR".into(),
                                message,
                            })
                            .await;
                    }
                    SttEvent::Closed => {
                        // ASR session shut down — let the reader loop exit.
                        break;
                    }
                }
            }
        });

        // Stop forwarder.
        let asr_for_stop = Arc::clone(&asr);
        tokio::spawn(async move {
            let _ = stop_rx.recv().await;
            asr_for_stop.close().await;
            translator_handle.abort();
        });

        Ok(Self {
            audio_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            session_id,
            stop_tx,
        })
    }

    pub async fn send_audio(&self, pcm: Vec<u8>) -> anyhow::Result<()> {
        self.audio_tx
            .send(pcm)
            .await
            .map_err(|_| anyhow::anyhow!("Gemma simul session audio channel closed"))
    }

    pub async fn stop(&self) {
        let _ = self.stop_tx.send(()).await;
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_emits_session_ready() {
        let cfg = GemmaSimulConfig {
            session_id: "wrap-test".to_string(),
            source_lang: LanguageCode::Ko,
            base_url: "http://127.0.0.1:1".to_string(), // unreachable on purpose
            model: "gemma4:e4b".to_string(),
        };
        let session = GemmaSimulSession::start(cfg).await.unwrap();
        let evt = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            session.event_rx.lock().await.recv(),
        )
        .await
        .expect("ready arrives within timeout")
        .expect("channel yields message");
        match evt {
            ServerMessage::SessionReady { session_id, .. } => {
                assert_eq!(session_id, "wrap-test");
            }
            other => panic!("expected SessionReady, got {other:?}"),
        }
    }
}
