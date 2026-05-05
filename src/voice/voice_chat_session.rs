//! Rust-native voice-chat session — replaces the LiveKit + Python
//! agent stack with a single in-process pipeline:
//!
//! ```text
//!  Client mic ─▸ audio_chunk frames ─▸ ChatSession
//!                                          │
//!                                          ▼
//!                               Gemma 4 ASR (on-device, free)
//!                                          │
//!                                          ▼
//!                       voice_chat_pipeline self-validation
//!                       │                   │
//!                       │  ── route: re-ask ─▸ localized phrase ─▸ TTS
//!                       │                                            │
//!                       └── route: answer ─▸ LLM (user key or       ▼
//!                                          operator Gemini Flash)   AudioOut
//!                                          │                         │
//!                                          ▼                         ▼
//!                                       AssistantText  ─────────────┘
//! ```
//!
//! ## Design choices
//!
//! * **Single process, no Python.** All pieces are crates already
//!   compiled into the gateway binary; voice chat shares the same
//!   process boundary as the agent loop. No separate service to
//!   deploy, no IPC, no language switch.
//!
//! * **No LiveKit.** The transport is a plain WebSocket (handled by
//!   `gateway/ws.rs::handle_voice_chat_socket`), the same shape the
//!   interpretation path uses today. Lets the Tauri client drop the
//!   `livekit-client` SDK and use the browser's WebSocket directly.
//!
//! * **TTS fallback chain.** Typecast first (Tier A — own-voice
//!   clone, paid, online); on failure or when the user is offline
//!   the session falls through to local Tier B/C engines via
//!   `tts_router`. This honors the user's "산악지역에서도 작동" goal:
//!   if Wi-Fi drops mid-conversation, Kokoro keeps producing audio
//!   in some voice rather than silence.
//!
//! * **Operator-key billing.** When the user has not configured a
//!   cloud LLM key the session uses `ProxyProvider` (same path as
//!   text chat) which bills the operator's Gemini Flash usage at
//!   2.2× — see `crate::billing::llm_router`. Never silently
//!   short-circuits with an apology.
//!
//! * **Per-turn retry counter.** The validation staircase
//!   (`AskUserToRepeat` → `ConfirmInterpretation` → `ComplexLlm`)
//!   is keyed by the *speaker's detected language* per turn so
//!   bidirectional sessions in the future will Just Work.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::deepgram_stt::SttEvent;
use super::events_chat::{ChatClientMessage, ChatServerMessage};
use super::gemma_asr::{GemmaAsrConfig, GemmaAsrSession, DEFAULT_OLLAMA_URL};
use super::pipeline::LanguageCode;
use super::voice_chat_pipeline::{QueryRoute, SttResult, VoiceChatPipeline};
use super::voice_messages::{
    ask_user_to_repeat, confirm_interpretation_fallback, confirm_interpretation_prefix,
};
use crate::providers::Provider;

// ── Configuration ────────────────────────────────────────────────

/// Configuration for one voice-chat session.
#[derive(Clone)]
pub struct VoiceChatSessionConfig {
    /// Unique session id (mirrors what the WS protocol uses).
    pub session_id: String,
    /// Ollama base URL for Gemma ASR. Empty string falls back to the
    /// crate-default localhost endpoint.
    pub gemma_base_url: String,
    /// Gemma 4 audio-capable tag (`"gemma4:e4b"` / `"gemma4:e2b"`).
    /// Resolved by the gateway via `LocalLlmConfig` per device tier.
    pub gemma_model: String,
    /// Speaker's session-level language hint. Used as the
    /// detection fallback for short utterances. `None` falls
    /// through to `LanguageCode::En` per the
    /// `voice_chat_pipeline` contract.
    pub source_lang: Option<LanguageCode>,
    /// LLM provider for the assistant reply. Resolved by the gateway
    /// per the operator-key fallback contract — see
    /// `crate::voice::voice_chat_pipeline` module docs.
    pub llm: Arc<dyn Provider>,
    /// LLM model id to use with `llm.chat_with_system(...)`.
    pub llm_model: String,
    /// Optional Typecast TTS — when present and online, used as
    /// the primary TTS path (Tier A). When `None` or after a call
    /// failure we fall through to the local TTS router.
    pub typecast: Option<TypecastTtsConfig>,
    /// Local TTS fallback router (Kokoro / CosyVoice). Always used
    /// when Typecast is unavailable; required for the offline use
    /// case the user explicitly called out (산악지역 등).
    pub local_tts: Option<Arc<crate::voice::tts_router::TtsRouter>>,
}

/// Typecast TTS settings — mirrored from `TypecastInterpConfig` so
/// the two voice paths share the same voice-resolution rules.
#[derive(Clone, Debug)]
pub struct TypecastTtsConfig {
    pub api_key: String,
    /// Voice clone resource id (when the user uploaded their own
    /// voice). When set, used in preference to `fallback_voice_id`.
    pub voice_clone_id: Option<String>,
    /// Pre-resolved Typecast voice id (gateway uses
    /// `select_fallback_voice_id` to populate this once per
    /// session). Required when `voice_clone_id` is `None`.
    pub fallback_voice_id: Option<String>,
}

// ── Public session handle ────────────────────────────────────────

/// One running voice-chat session. Construct via
/// `VoiceChatSession::start`; feed mic audio in via
/// `send_client_message`; read server events via `event_rx`.
pub struct VoiceChatSession {
    audio_tx: mpsc::Sender<Vec<u8>>,
    end_of_speech_tx: mpsc::Sender<()>,
    pub event_rx: Arc<Mutex<mpsc::Receiver<ChatServerMessage>>>,
    session_id: String,
    stop_tx: mpsc::Sender<()>,
}

impl VoiceChatSession {
    /// Start a new voice-chat session.
    ///
    /// `validator` is the optional `voice_chat_pipeline` instance the
    /// gateway already constructed for self-validation. When `None`
    /// the session skips the staircase and sends every Gemma
    /// transcript straight to the LLM (graceful degradation).
    pub async fn start(
        config: VoiceChatSessionConfig,
        validator: Option<Arc<VoiceChatPipeline>>,
    ) -> Result<Self> {
        let session_id = config.session_id.clone();

        // Resolve the Ollama endpoint + model and start the
        // streaming Gemma ASR session.
        let gemma_base_url = if config.gemma_base_url.trim().is_empty() {
            DEFAULT_OLLAMA_URL.to_string()
        } else {
            config.gemma_base_url.clone()
        };
        let asr_config = GemmaAsrConfig {
            session_id: session_id.clone(),
            base_url: gemma_base_url.clone(),
            model: config.gemma_model.clone(),
            language_hint: config.source_lang.map(|l| l.as_str().to_string()),
            ..Default::default()
        };

        tracing::info!(
            session_id = %session_id,
            gemma_model = %config.gemma_model,
            llm_model = %config.llm_model,
            has_typecast = config.typecast.is_some(),
            has_local_tts = config.local_tts.is_some(),
            "Starting Rust-native voice-chat session"
        );

        let asr_session = GemmaAsrSession::start(asr_config)
            .await
            .context("starting Gemma ASR session for voice chat")?;

        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (end_of_speech_tx, mut end_of_speech_rx) = mpsc::channel::<()>(8);
        let (event_tx, event_rx) = mpsc::channel::<ChatServerMessage>(256);
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

        let asr_session = Arc::new(asr_session);
        let config = Arc::new(config);

        // Audio forwarder: client mic → Gemma ASR.
        let asr_for_audio = Arc::clone(&asr_session);
        tokio::spawn(async move {
            let mut audio_rx = audio_rx;
            while let Some(pcm) = audio_rx.recv().await {
                if let Err(e) = asr_for_audio.send_audio(pcm).await {
                    tracing::warn!(error = %e, "voice-chat: audio forward to Gemma failed");
                    break;
                }
            }
            asr_for_audio.close().await;
        });

        // End-of-speech bridge: forward explicit hints into the ASR
        // session by closing its audio channel via close(). For now
        // we only log them — Gemma's RMS VAD already commits on
        // silence, so this path is purely advisory.
        tokio::spawn(async move {
            while end_of_speech_rx.recv().await.is_some() {
                tracing::debug!("voice-chat: end_of_speech hint received (advisory)");
            }
        });

        // Main event processor: Gemma SttEvent → validate → LLM → TTS.
        let asr_for_events = Arc::clone(&asr_session);
        let event_tx_for_events = event_tx.clone();
        let cfg_for_events = Arc::clone(&config);
        let validator_for_events = validator.clone();
        let sid_for_events = session_id.clone();
        let processor_handle = tokio::spawn(async move {
            event_processor(
                asr_for_events,
                event_tx_for_events,
                sid_for_events,
                cfg_for_events,
                validator_for_events,
            )
            .await;
        });

        // Stop coordinator: when the WS closes or the client sends
        // SessionStop, abort the processor and close the ASR.
        let asr_for_stop = Arc::clone(&asr_session);
        let event_tx_for_stop = event_tx.clone();
        let sid_for_stop = session_id.clone();
        tokio::spawn(async move {
            let _ = stop_rx.recv().await;
            asr_for_stop.close().await;
            processor_handle.abort();
            let _ = event_tx_for_stop
                .send(ChatServerMessage::SessionEnded {
                    session_id: sid_for_stop,
                })
                .await;
        });

        // Initial ready event.
        let _ = event_tx
            .send(ChatServerMessage::SessionReady {
                session_id: session_id.clone(),
            })
            .await;

        Ok(Self {
            audio_tx,
            end_of_speech_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            session_id,
            stop_tx,
        })
    }

    /// Push a parsed `ChatClientMessage` from the WebSocket into
    /// the session. Handles routing to the right channel
    /// (audio buffer, end-of-speech bridge, stop signal).
    pub async fn send_client_message(&self, msg: ChatClientMessage) -> Result<()> {
        match msg {
            ChatClientMessage::AudioChunk { pcm16le, .. } => {
                use base64::Engine;
                let pcm = base64::engine::general_purpose::STANDARD
                    .decode(&pcm16le)
                    .context("audio_chunk pcm16le base64 decode")?;
                self.audio_tx
                    .send(pcm)
                    .await
                    .map_err(|_| anyhow::anyhow!("audio channel closed"))
            }
            ChatClientMessage::EndOfSpeech { .. } => {
                let _ = self.end_of_speech_tx.send(()).await;
                Ok(())
            }
            ChatClientMessage::SessionStop { .. } => {
                let _ = self.stop_tx.send(()).await;
                Ok(())
            }
            ChatClientMessage::SessionStart { .. } => {
                // SessionStart is the WS handler's responsibility,
                // not the session's; the session itself is already
                // running by the time this could be observed.
                Ok(())
            }
        }
    }

    pub async fn stop(&self) {
        let _ = self.stop_tx.send(()).await;
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

// ── Internal: event processor ────────────────────────────────────

/// Drive one user→assistant turn for each Gemma `SttEvent::Final`.
async fn event_processor(
    asr: Arc<GemmaAsrSession>,
    event_tx: mpsc::Sender<ChatServerMessage>,
    session_id: String,
    config: Arc<VoiceChatSessionConfig>,
    validator: Option<Arc<VoiceChatPipeline>>,
) {
    // Per-speaker retry counter for the validation staircase.
    // Single-language voice chat never strictly needs the HashMap
    // (English-only sessions touch one bucket), but mirroring the
    // interpretation pipeline's structure means future bidirectional
    // voice-chat work will be a no-op upgrade.
    let mut voice_retry_counts: HashMap<LanguageCode, u8> = HashMap::new();

    let mut rx = asr.event_rx.lock().await;
    while let Some(event) = rx.recv().await {
        match event {
            SttEvent::Ready { .. } | SttEvent::SpeechStarted { .. } | SttEvent::Partial { .. } => {
                // Voice chat does not surface partials — it waits
                // for the full utterance before doing anything.
            }
            SttEvent::Final { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                handle_user_turn(&text, &session_id, &event_tx, &config, &validator, &mut voice_retry_counts)
                    .await;
            }
            SttEvent::UtteranceEnd { .. } => {
                // The Final event already handled this turn.
            }
            SttEvent::Error { message } => {
                let _ = event_tx
                    .send(ChatServerMessage::Error {
                        session_id: session_id.clone(),
                        code: "GEMMA_STT_ERROR".into(),
                        message,
                    })
                    .await;
            }
            SttEvent::Closed => break,
        }
    }
}

/// Run the full validate → LLM → TTS pipeline for one user turn.
async fn handle_user_turn(
    user_text: &str,
    session_id: &str,
    event_tx: &mpsc::Sender<ChatServerMessage>,
    config: &VoiceChatSessionConfig,
    validator: &Option<Arc<VoiceChatPipeline>>,
    voice_retry_counts: &mut HashMap<LanguageCode, u8>,
) {
    // Pre-detect language so we can label the user transcript even
    // when validation is disabled or fails.
    let default_lang = config.source_lang.unwrap_or(LanguageCode::En);
    let pre_detected = crate::voice::pipeline::detect_language(user_text, default_lang);

    // Surface the user's own transcript first — chat UIs render
    // the speaker's bubble immediately, before the assistant
    // starts replying.
    let _ = event_tx
        .send(ChatServerMessage::UserTranscript {
            session_id: session_id.to_string(),
            text: user_text.to_string(),
            detected_language: pre_detected.as_str().to_string(),
        })
        .await;

    // Run validation (when configured). Validation failures fall
    // through to the LLM path — we never want to block a real
    // conversation on a misconfiguration.
    let pre_retry = voice_retry_counts.get(&pre_detected).copied().unwrap_or(0);
    let route_decision = if let Some(v) = validator {
        let stt = SttResult {
            text: user_text.to_string(),
            confidence: 1.0,
            processing_time_ms: 0,
            voice_retry_count: pre_retry,
            default_language: Some(default_lang),
        };
        match v.validate_only(&stt).await {
            Ok(validation) => Some(validation),
            Err(e) => {
                tracing::warn!(error = %e, "voice-chat: validation failed; proceeding with LLM");
                None
            }
        }
    } else {
        None
    };

    let (proceed_to_llm, detected_lang) = match route_decision {
        Some(validation) => {
            let lang = validation.detected_language;
            match validation.route {
                QueryRoute::SimpleGemma | QueryRoute::ComplexLlm => {
                    voice_retry_counts.insert(lang, 0);
                    (true, lang)
                }
                QueryRoute::AskUserToRepeat => {
                    let entry = voice_retry_counts.entry(lang).or_insert(0);
                    *entry = entry.saturating_add(1);
                    let phrase = ask_user_to_repeat(lang).to_string();
                    let _ = event_tx
                        .send(ChatServerMessage::ReAsk {
                            session_id: session_id.to_string(),
                            route: "ask_user_to_repeat".into(),
                            message: phrase.clone(),
                            voice_retry_count: *entry,
                        })
                        .await;
                    synthesize_phrase(config, &phrase, lang.as_str(), session_id, event_tx).await;
                    let _ = event_tx
                        .send(ChatServerMessage::TurnComplete {
                            session_id: session_id.to_string(),
                        })
                        .await;
                    return;
                }
                QueryRoute::ConfirmInterpretation => {
                    let entry = voice_retry_counts.entry(lang).or_insert(0);
                    *entry = entry.saturating_add(1);
                    let paraphrase = validation.interpreted_meaning.trim();
                    let phrase = if paraphrase.is_empty() {
                        confirm_interpretation_fallback(lang).to_string()
                    } else {
                        format!(
                            "{prefix} '{paraphrase}'",
                            prefix = confirm_interpretation_prefix(lang)
                        )
                    };
                    let _ = event_tx
                        .send(ChatServerMessage::ReAsk {
                            session_id: session_id.to_string(),
                            route: "confirm_interpretation".into(),
                            message: phrase.clone(),
                            voice_retry_count: *entry,
                        })
                        .await;
                    synthesize_phrase(config, &phrase, lang.as_str(), session_id, event_tx).await;
                    let _ = event_tx
                        .send(ChatServerMessage::TurnComplete {
                            session_id: session_id.to_string(),
                        })
                        .await;
                    return;
                }
            }
        }
        None => (true, pre_detected),
    };

    if !proceed_to_llm {
        return;
    }

    // ── LLM step ─────────────────────────────────────────────────
    let answer = match call_llm(user_text, detected_lang, config).await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "voice-chat: LLM call failed");
            let _ = event_tx
                .send(ChatServerMessage::Error {
                    session_id: session_id.to_string(),
                    code: "LLM_CALL_FAILED".into(),
                    message: e.to_string(),
                })
                .await;
            let _ = event_tx
                .send(ChatServerMessage::TurnComplete {
                    session_id: session_id.to_string(),
                })
                .await;
            return;
        }
    };

    // Surface the assistant text BEFORE the audio so UIs can render
    // the bubble immediately while TTS is still synthesizing.
    let _ = event_tx
        .send(ChatServerMessage::AssistantText {
            session_id: session_id.to_string(),
            text: answer.clone(),
        })
        .await;

    // ── TTS step ─────────────────────────────────────────────────
    synthesize_phrase(config, &answer, detected_lang.as_str(), session_id, event_tx).await;

    let _ = event_tx
        .send(ChatServerMessage::TurnComplete {
            session_id: session_id.to_string(),
        })
        .await;
}

/// One LLM call. System prompt asks the model to reply in the
/// speaker's detected language so the TTS step has natural input.
async fn call_llm(
    user_text: &str,
    speaker_lang: LanguageCode,
    config: &VoiceChatSessionConfig,
) -> Result<String> {
    let system_prompt = format!(
        "You are a friendly voice assistant. The user just spoke to you in {lang}. \
         Reply briefly (1-3 sentences) in {lang}, in a natural conversational tone, \
         as if you were talking out loud. Do not include markdown, code blocks, \
         or list formatting — your reply will be spoken aloud.",
        lang = speaker_lang.display_name(),
    );

    let response = config
        .llm
        .chat_with_system(Some(&system_prompt), user_text, &config.llm_model, 0.7)
        .await
        .context("voice-chat LLM chat_with_system call")?;

    let trimmed = response.trim();
    if trimmed.is_empty() {
        anyhow::bail!("LLM returned empty reply");
    }
    Ok(trimmed.to_string())
}

// ── TTS with fallback chain ──────────────────────────────────────

/// Synthesize a phrase via the TTS chain:
///   1. Typecast (Tier A) — own-voice clone, paid, requires online.
///   2. Local TTS router (Tier B/C) — CosyVoice or Kokoro, offline.
/// On Typecast failure (network down, API error, no voice id) we
/// transparently fall through to the local router so the user
/// always hears something — including in the offline-mountain
/// scenario the user explicitly called out.
async fn synthesize_phrase(
    config: &VoiceChatSessionConfig,
    phrase: &str,
    language_code_str: &str,
    session_id: &str,
    event_tx: &mpsc::Sender<ChatServerMessage>,
) {
    // Try Tier A (Typecast).
    if let Some(typecast_cfg) = &config.typecast {
        match typecast_synthesize(typecast_cfg, phrase, language_code_str).await {
            Ok((pcm, sample_rate)) => {
                emit_audio_chunks(session_id, &pcm, sample_rate, event_tx).await;
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "voice-chat: Typecast TTS failed; falling back to local TTS"
                );
            }
        }
    }

    // Fall through to Tier B/C via local router.
    if let Some(router) = &config.local_tts {
        match local_synthesize(router, phrase, language_code_str).await {
            Ok((pcm, sample_rate)) => {
                emit_audio_chunks(session_id, &pcm, sample_rate, event_tx).await;
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "voice-chat: local TTS also failed; phrase delivered as text only"
                );
            }
        }
    } else {
        tracing::warn!(
            session_id = %session_id,
            "voice-chat: no TTS engine available (Typecast off, local router unset); \
             phrase delivered as text only"
        );
    }
}

/// Call Typecast TTS via the existing helper. Returns PCM16LE +
/// sample-rate (Typecast ssfm-v30 emits 44.1 kHz).
async fn typecast_synthesize(
    cfg: &TypecastTtsConfig,
    phrase: &str,
    language_iso3_or_bcp47: &str,
) -> Result<(Vec<u8>, u32)> {
    let voice_id = cfg
        .voice_clone_id
        .as_deref()
        .or(cfg.fallback_voice_id.as_deref())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no Typecast voice id"))?;

    // The interp module's helper expects a Typecast ISO-639-3-ish
    // language string; we keep this routine string-agnostic and let
    // the caller pass either form. Typecast accepts BCP-47 too in
    // most cases, so a mismatched input fails the API call and we
    // fall through to local TTS.
    let pcm = super::typecast_interp::call_typecast_tts_synthesize(
        &cfg.api_key,
        voice_id,
        phrase,
        language_iso3_or_bcp47,
        cfg.voice_clone_id.as_deref(),
    )
    .await?;
    Ok((pcm, 44_100))
}

/// Pick a tier via `tts_router::decide` (offline-friendly path —
/// we want CosyVoice when the device can run it, otherwise Kokoro,
/// which always ships) and synthesize.
async fn local_synthesize(
    router: &Arc<crate::voice::tts_router::TtsRouter>,
    phrase: &str,
    language_code: &str,
) -> Result<(Vec<u8>, u32)> {
    use crate::voice::tts_engine::{EmotionHint, TtsEngine};
    use crate::voice::tts_router::{decide, RoutingContext, Tier};

    // Build a routing context biased toward the offline fallback
    // case: the caller already tried Tier A (Typecast) and that
    // failed, so we deliberately mark `online=false` here even if
    // network is up — we want a local synthesis result.
    let mut ctx = RoutingContext {
        online: false,
        strict_local: true,
        ..RoutingContext::default()
    };
    router.refresh_engine_health(&mut ctx).await;
    let tier = decide(&ctx);

    // Pick the first voice each engine reports as its "default"
    // persona. Voice picking by name lives on the picker UI; this
    // synthesis fallback just needs *some* voice id so the engine
    // does not return an empty buffer.
    let voice_id: String = match tier {
        Tier::B => router
            .cosyvoice()
            .and_then(|c| {
                let voices = c.list_voices();
                voices.first().map(|v| v.id.clone())
            })
            .unwrap_or_default(),
        _ => router
            .kokoro()
            .and_then(|k| {
                let voices = k.list_voices();
                voices.first().map(|v| v.id.clone())
            })
            .unwrap_or_default(),
    };

    let result = router
        .synthesize(tier, phrase, &voice_id, language_code, &EmotionHint::default())
        .await
        .context("local TTS synthesize")?;
    Ok((result.pcm, result.sample_rate))
}

/// Stream PCM as ~100 ms `AudioOut` chunks so the client can begin
/// playback immediately rather than waiting for the whole reply.
async fn emit_audio_chunks(
    session_id: &str,
    pcm: &[u8],
    sample_rate: u32,
    event_tx: &mpsc::Sender<ChatServerMessage>,
) {
    // 100 ms of mono PCM16 at any sample rate = sample_rate / 10
    // samples = sample_rate / 5 bytes.
    let chunk_size = ((sample_rate as usize) / 5).max(1);
    use base64::Engine;
    for (seq, chunk) in (0u64..).zip(pcm.chunks(chunk_size)) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
        let _ = event_tx
            .send(ChatServerMessage::AudioOut {
                session_id: session_id.to_string(),
                seq,
                sample_rate,
                pcm16le: b64,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the staircase counter logic in `handle_user_turn`,
    /// kept as a small helper so a unit test can drive it without
    /// constructing a full session. If the inline logic in
    /// `handle_user_turn` changes, this helper must change in
    /// lockstep — same convention as `typecast_interp::tests`.
    fn simulate_route_in_chat(
        counts: &mut HashMap<LanguageCode, u8>,
        route: QueryRoute,
        lang: LanguageCode,
    ) {
        match route {
            QueryRoute::SimpleGemma | QueryRoute::ComplexLlm => {
                counts.insert(lang, 0);
            }
            QueryRoute::AskUserToRepeat | QueryRoute::ConfirmInterpretation => {
                let entry = counts.entry(lang).or_insert(0);
                *entry = entry.saturating_add(1);
            }
        }
    }

    #[test]
    fn chat_retry_counter_increments_then_resets_after_assistant_reply() {
        let mut counts: HashMap<LanguageCode, u8> = HashMap::new();
        simulate_route_in_chat(&mut counts, QueryRoute::AskUserToRepeat, LanguageCode::Ko);
        simulate_route_in_chat(
            &mut counts,
            QueryRoute::ConfirmInterpretation,
            LanguageCode::Ko,
        );
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(2));
        // User confirmed → answer goes through normally → reset.
        simulate_route_in_chat(&mut counts, QueryRoute::ComplexLlm, LanguageCode::Ko);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(0));
    }

    #[test]
    fn chat_retry_counter_isolated_per_speaker_language() {
        let mut counts: HashMap<LanguageCode, u8> = HashMap::new();
        simulate_route_in_chat(&mut counts, QueryRoute::AskUserToRepeat, LanguageCode::Ko);
        simulate_route_in_chat(&mut counts, QueryRoute::SimpleGemma, LanguageCode::En);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(1));
        assert_eq!(counts.get(&LanguageCode::En).copied(), Some(0));
    }

    #[test]
    fn audio_chunk_size_is_about_100ms_at_24khz() {
        // 24000 / 5 = 4800 bytes per chunk → 100 ms of 16-bit mono at 24 kHz.
        let chunk_size = ((24_000_u32 as usize) / 5).max(1);
        assert_eq!(chunk_size, 4_800);
    }
}
