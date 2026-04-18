//! STT+LLM+TTS interpretation session (Mode 2).
//!
//! Combines Deepgram STT with LLM-based translation and Typecast TTS
//! to produce interpreted speech in the user's cloned voice (or a
//! best-matching fallback voice).
//!
//! ## Architecture
//!
//! ```text
//! Client mic ─▸ audio_chunk ─▸ TypecastInterpSession
//!                                    │
//!                       Deepgram STT (segmentation)
//!                                    │
//!                            CommitSrc (source text)
//!                                    │
//!                          LLM translation call
//!                                    │
//!                            CommitTgt (translated text)
//!                                    │
//!                          Typecast TTS synthesis
//!                       (voice clone or auto-matched voice)
//!                                    │
//!                            AudioOut → Client speaker
//! ```
//!
//! ## Voice Selection Priority
//!
//! 1. **Voice clone** (`voice_clone_id`): user's cloned voice via Typecast
//!    voice cloning API (`tts_mode: "audio_file"`).
//! 2. **Auto-matched fallback**: selects the best Typecast voice based on
//!    user's gender, age, and target language from the cached voice list.

use base64::Engine;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::deepgram_stt::{DeepgramConfig, DeepgramSttSession, SttEvent};
use super::events::ServerMessage;
use super::pipeline::{LanguageCode, VoiceAge, VoiceGender};
use super::simul::{SegmentationConfig, SegmentationEngine};

// ── Configuration ─────────────────────────────────────────────────

/// Configuration for a Typecast-based interpretation session.
#[derive(Debug, Clone)]
pub struct TypecastInterpConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Deepgram API key (for STT).
    pub deepgram_api_key: String,
    /// Deepgram model (e.g. "nova-3").
    pub deepgram_model: String,
    /// Source language code (what the user speaks).
    pub source_lang: LanguageCode,
    /// Target language code (translation output).
    pub target_lang: LanguageCode,
    /// Typecast API key (for TTS).
    pub typecast_api_key: String,
    /// Typecast voice clone ID (speak_resource_id). When Some, uses
    /// cloned voice; when None, falls back to auto-matched voice.
    pub voice_clone_id: Option<String>,
    /// User's voice gender for auto-matching.
    pub voice_gender: VoiceGender,
    /// User's voice age for auto-matching.
    pub voice_age: VoiceAge,
    /// Fallback Typecast voice_id (pre-resolved from voice list).
    /// Set by the gateway after querying /api/voices/list.
    pub fallback_voice_id: Option<String>,
    /// LLM API key for translation (Gemini / OpenAI / etc.).
    pub llm_api_key: String,
    /// LLM model name (e.g. "gemini-3.1-flash-lite-preview").
    pub llm_model: String,
    /// LLM API base URL.
    pub llm_base_url: String,
    /// Segmentation configuration.
    pub segmentation: SegmentationConfig,
    /// Bidirectional mode.
    pub bidirectional: bool,
}

// ── Voice matching ────────────────────────────────────────────────

/// Map LanguageCode to Typecast ISO 639-3 code.
pub fn lang_to_typecast_iso3(lang: LanguageCode) -> &'static str {
    match lang {
        LanguageCode::Ko => "kor",
        LanguageCode::En => "eng",
        LanguageCode::Ja => "jpn",
        LanguageCode::Zh | LanguageCode::ZhTw => "cmn",
        LanguageCode::Es => "spa",
        LanguageCode::Fr => "fra",
        LanguageCode::De => "deu",
        LanguageCode::Pt => "por",
        LanguageCode::It => "ita",
        LanguageCode::Vi => "vie",
        LanguageCode::Th => "tha",
        LanguageCode::Id => "ind",
        LanguageCode::Hi => "hin",
        LanguageCode::Ar => "ara",
        LanguageCode::Tr => "tur",
        LanguageCode::Ru => "rus",
        LanguageCode::Pl => "pol",
        LanguageCode::Nl => "nld",
        LanguageCode::Sv => "swe",
        LanguageCode::Da => "dan",
        LanguageCode::Cs => "ces",
        LanguageCode::Uk => "ukr",
        LanguageCode::Ms => "msa",
        LanguageCode::Tl => "fil",
    }
}

/// Use-case categories preferred for interpretation (natural speech).
/// Game voices are excluded — they sound unnatural for interpretation.
const PREFERRED_USE_CASES: &[&str] = &[
    "Conversational",
    "News Reporter",
    "Announcer",
    "Documentary",
    "Radio/Podcast",
    "Voicemail/Voice Assistant",
    "E-learning/Explainer",
];

/// Use-case categories excluded from interpretation voice matching.
const EXCLUDED_USE_CASES: &[&str] = &["Game", "Anime", "TikTok/Reels/Shorts"];

/// Score how well a Typecast voice matches the user profile.
///
/// Higher = better match. Returns 0 if gender doesn't match or voice
/// is from an excluded use-case category (Game, Anime, etc.).
pub fn voice_match_score(
    voice_gender: &str,
    voice_age: &str,
    voice_use_cases: &[String],
    user_gender: VoiceGender,
    user_age: VoiceAge,
) -> u32 {
    let mut score = 0u32;

    // Hard filter: gender must match
    let gender_match = match user_gender {
        VoiceGender::Male => voice_gender == "male",
        VoiceGender::Female => voice_gender == "female",
    };
    if !gender_match {
        return 0;
    }
    score += 10;

    // Hard filter: exclude game/anime/short-form voices
    let has_excluded = voice_use_cases
        .iter()
        .any(|uc| EXCLUDED_USE_CASES.iter().any(|ex| uc.contains(ex)));
    if has_excluded && voice_use_cases.len() == 1 {
        // Only exclude if the voice's sole use-case is excluded
        return 0;
    }

    // Age matching (soft score)
    let age_str = user_age.as_typecast_str();
    if voice_age == age_str {
        score += 10; // exact match
    } else {
        let age_order = |a: &str| -> i32 {
            match a {
                "child" => 0,
                "teenager" => 1,
                "young_adult" => 2,
                "middle_age" => 3,
                "elder" => 4,
                _ => 2,
            }
        };
        let diff = (age_order(voice_age) - age_order(age_str)).unsigned_abs();
        if diff == 1 {
            score += 5;
        } else if diff == 2 {
            score += 2;
        }
    }

    // Use-case bonus: prefer Conversational > Announcer/News > Documentary
    for uc in voice_use_cases {
        if uc == "Conversational" {
            score += 8; // best for natural interpretation
            break;
        } else if uc.contains("News") || uc.contains("Announcer") {
            score += 6; // clear pronunciation, good for interpretation
        } else if uc.contains("Documentary") || uc.contains("Podcast") {
            score += 4; // calm, natural tone
        } else if PREFERRED_USE_CASES.iter().any(|p| uc.contains(p)) {
            score += 2;
        }
    }

    score
}

// ── TTS call ──────────────────────────────────────────────────────

/// Call Typecast TTS API and return raw PCM16 audio bytes (44.1kHz mono).
/// Strips the 44-byte WAV header.
async fn typecast_tts_synthesize(
    api_key: &str,
    voice_id: &str,
    text: &str,
    language: &str,
    voice_clone_id: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::Client::new();

    let mut body = serde_json::json!({
        "voice_id": voice_id,
        "text": &text[..text.len().min(2000)],
        "model": "ssfm-v30",
        "language": language,
        "prompt": {
            "emotion_type": "smart",
            "emotion_intensity": 1.0
        },
        "output": {
            "audio_format": "wav",
            "audio_tempo": 1.0,
            "volume": 100
        }
    });

    // If voice clone ID is available, use audio_file TTS mode
    if let Some(clone_id) = voice_clone_id {
        body["tts_mode"] = serde_json::json!("audio_file");
        body["speak_resource_id"] = serde_json::json!(clone_id);
    }

    let resp = client
        .post("https://api.typecast.ai/v1/text-to-speech")
        .header("X-API-KEY", api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Typecast TTS error {status}: {err_text}");
    }

    let wav_bytes = resp.bytes().await?;
    // Strip 44-byte WAV header to get raw PCM16
    if wav_bytes.len() <= 44 {
        anyhow::bail!("Typecast TTS returned empty audio");
    }
    Ok(wav_bytes[44..].to_vec())
}

// ── LLM translation call (Gemini REST API) ───────────────────────

/// Gemini model used for real-time interpretation translation.
/// gemini-2.0-flash-lite is the fastest and cheapest option suitable
/// for phrase-level interpretation where low latency matters.
const GEMINI_TRANSLATE_MODEL: &str = "gemini-3.1-flash-lite-preview";

/// Translate text using the Gemini REST API (generateContent).
///
/// Uses `gemini-2.0-flash-lite` for cost efficiency and low latency.
/// Falls back to the configured model if `llm_model` is overridden.
async fn llm_translate(
    api_key: &str,
    _base_url: &str,
    model: &str,
    source_lang: LanguageCode,
    target_lang: LanguageCode,
    text: &str,
    bidirectional: bool,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();

    let system_prompt = if bidirectional {
        let detected = super::pipeline::detect_language(text, source_lang);
        let output_lang = if detected == target_lang {
            source_lang
        } else {
            target_lang
        };
        format!(
            "You are a real-time interpreter. Translate the following into {}. \
             Output ONLY the translation, no explanations.",
            output_lang.display_name()
        )
    } else {
        format!(
            "You are a real-time interpreter. Translate the following from {} to {}. \
             Output ONLY the translation, no explanations.",
            source_lang.display_name(),
            target_lang.display_name()
        )
    };

    // Use configured model or default to gemini-2.0-flash-lite
    let effective_model = if model.is_empty() {
        GEMINI_TRANSLATE_MODEL
    } else {
        model
    };

    // Gemini REST API: generateContent endpoint
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{effective_model}:generateContent?key={api_key}"
    );

    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [{ "text": system_prompt }]
        },
        "contents": [{
            "parts": [{ "text": text }]
        }],
        "generationConfig": {
            "temperature": 0.3,
            "maxOutputTokens": 1024
        }
    });

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Gemini translation error {status}: {err_text}");
    }

    let json: serde_json::Value = resp.json().await?;
    let translated = json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    if translated.is_empty() {
        anyhow::bail!("Gemini returned empty translation");
    }

    Ok(translated)
}

// ── Session handle ────────────────────────────────────────────────

/// Handle to a running Typecast interpretation session.
pub struct TypecastInterpSession {
    audio_tx: mpsc::Sender<Vec<u8>>,
    pub event_rx: Arc<Mutex<mpsc::Receiver<ServerMessage>>>,
    session_id: String,
    stop_tx: mpsc::Sender<()>,
}

impl TypecastInterpSession {
    /// Start a new STT+LLM+TTS interpretation session.
    pub async fn start(config: TypecastInterpConfig) -> anyhow::Result<Self> {
        let session_id = config.session_id.clone();

        // Build Deepgram config
        let dg_lang = if config.source_lang == LanguageCode::Ko
            || config.source_lang == LanguageCode::En
        {
            super::deepgram_stt::language_code_to_deepgram(&config.source_lang).to_string()
        } else {
            "multi".to_string()
        };

        let dg_config = DeepgramConfig {
            api_key: config.deepgram_api_key.clone(),
            model: config.deepgram_model.clone(),
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
            target = config.target_lang.as_str(),
            has_voice_clone = config.voice_clone_id.is_some(),
            "Starting Typecast interpretation session (STT+LLM+TTS)"
        );

        let dg_session = DeepgramSttSession::connect(session_id.clone(), &dg_config).await?;

        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (event_tx, event_rx) = mpsc::channel::<ServerMessage>(256);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);

        let segmentation = Arc::new(Mutex::new(SegmentationEngine::new(config.segmentation.clone())));
        let dg_session = Arc::new(dg_session);
        let config = Arc::new(config);

        // Spawn audio forwarder
        let dg_for_audio = Arc::clone(&dg_session);
        tokio::spawn(async move {
            Self::audio_forwarder(audio_rx, dg_for_audio).await;
        });

        // Spawn event processor (STT → segmentation → LLM → TTS)
        let dg_for_events = Arc::clone(&dg_session);
        let seg_for_events = Arc::clone(&segmentation);
        let event_tx_events = event_tx.clone();
        let sid_events = session_id.clone();
        let cfg_events = Arc::clone(&config);
        tokio::spawn(async move {
            Self::event_processor(
                dg_for_events,
                seg_for_events,
                event_tx_events,
                sid_events,
                cfg_events,
            )
            .await;
        });

        // Spawn tick timer
        let seg_for_tick = Arc::clone(&segmentation);
        let event_tx_tick = event_tx.clone();
        let sid_tick = session_id.clone();
        let cfg_tick = Arc::clone(&config);
        tokio::spawn(async move {
            Self::tick_timer(seg_for_tick, event_tx_tick, stop_rx, sid_tick, cfg_tick).await;
        });

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

    pub async fn send_audio(&self, pcm_data: Vec<u8>) -> anyhow::Result<()> {
        self.audio_tx
            .send(pcm_data)
            .await
            .map_err(|_| anyhow::anyhow!("Session audio channel closed"))
    }

    pub async fn recv_event(&self) -> Option<ServerMessage> {
        self.event_rx.lock().await.recv().await
    }

    pub async fn stop(&self) {
        let _ = self.stop_tx.send(()).await;
    }

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
        let _ = dg.finalize().await;
    }

    // ── Internal: translate + synthesize a committed segment ──────

    async fn translate_and_speak(
        config: &TypecastInterpConfig,
        commit_id: u64,
        source_text: &str,
        session_id: &str,
        event_tx: &mpsc::Sender<ServerMessage>,
    ) {
        // 1. LLM translation
        let translated = match llm_translate(
            &config.llm_api_key,
            &config.llm_base_url,
            &config.llm_model,
            config.source_lang,
            config.target_lang,
            source_text,
            config.bidirectional,
        )
        .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(session_id = %session_id, error = %e, "LLM translation failed");
                let _ = event_tx
                    .send(ServerMessage::Error {
                        session_id: session_id.to_string(),
                        code: "LLM_TRANSLATE_ERROR".into(),
                        message: e.to_string(),
                    })
                    .await;
                return;
            }
        };

        // Send CommitTgt
        let _ = event_tx
            .send(ServerMessage::CommitTgt {
                session_id: session_id.to_string(),
                commit_id,
                text: translated.clone(),
            })
            .await;

        // 2. Typecast TTS synthesis
        let voice_id = config
            .voice_clone_id
            .as_deref()
            .or(config.fallback_voice_id.as_deref())
            .unwrap_or("");

        if voice_id.is_empty() {
            tracing::warn!(session_id = %session_id, "No voice ID for TTS — skipping audio");
            return;
        }

        let tgt_iso3 = lang_to_typecast_iso3(config.target_lang);

        match typecast_tts_synthesize(
            &config.typecast_api_key,
            voice_id,
            &translated,
            tgt_iso3,
            config.voice_clone_id.as_deref(),
        )
        .await
        {
            Ok(pcm_data) => {
                // Split into ~100ms chunks at 44100Hz 16-bit mono = 8820 bytes
                let chunk_size = 8820;
                for (seq, chunk) in (0u64..).zip(pcm_data.chunks(chunk_size)) {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
                    let _ = event_tx
                        .send(ServerMessage::AudioOut {
                            session_id: session_id.to_string(),
                            seq,
                            pcm16le: b64,
                        })
                        .await;
                }
            }
            Err(e) => {
                tracing::error!(session_id = %session_id, error = %e, "Typecast TTS failed");
                let _ = event_tx
                    .send(ServerMessage::Error {
                        session_id: session_id.to_string(),
                        code: "TTS_ERROR".into(),
                        message: e.to_string(),
                    })
                    .await;
            }
        }
    }

    // ── Internal: event processor ─────────────────────────────────

    async fn event_processor(
        dg: Arc<DeepgramSttSession>,
        segmentation: Arc<Mutex<SegmentationEngine>>,
        event_tx: mpsc::Sender<ServerMessage>,
        session_id: String,
        config: Arc<TypecastInterpConfig>,
    ) {
        loop {
            let event = match dg.recv_event().await {
                Some(e) => e,
                None => break,
            };

            match event {
                SttEvent::Ready { .. } => {}

                SttEvent::Partial { text, .. } => {
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
                    let mut seg = segmentation.lock().await;
                    seg.append_partial(&text);

                    let _ = event_tx
                        .send(ServerMessage::PartialSrc {
                            session_id: session_id.clone(),
                            text: seg.partial_text().to_string(),
                            stable_prefix_len: seg.stable_prefix_len(),
                            is_final: speech_final,
                        })
                        .await;

                    // Collect committed segments
                    let mut commits = Vec::new();
                    while let Some(committed) = seg.try_commit() {
                        let _ = event_tx
                            .send(ServerMessage::CommitSrc {
                                session_id: session_id.clone(),
                                commit_id: committed.commit_id,
                                text: committed.text.clone(),
                            })
                            .await;
                        commits.push(committed);
                    }

                    if speech_final {
                        if let Some(committed) = seg.flush() {
                            let _ = event_tx
                                .send(ServerMessage::CommitSrc {
                                    session_id: session_id.clone(),
                                    commit_id: committed.commit_id,
                                    text: committed.text.clone(),
                                })
                                .await;
                            commits.push(committed);
                        }
                    }

                    // Release lock before async translation calls
                    drop(seg);

                    // Translate + TTS each committed segment
                    for committed in commits {
                        Self::translate_and_speak(
                            &config,
                            committed.commit_id,
                            &committed.text,
                            &session_id,
                            &event_tx,
                        )
                        .await;
                    }
                }

                SttEvent::SpeechStarted { .. } => {}

                SttEvent::UtteranceEnd { .. } => {
                    let mut seg = segmentation.lock().await;
                    let flushed = seg.flush_all();
                    drop(seg);

                    if let Some(committed) = flushed {
                        let _ = event_tx
                            .send(ServerMessage::CommitSrc {
                                session_id: session_id.clone(),
                                commit_id: committed.commit_id,
                                text: committed.text.clone(),
                            })
                            .await;

                        Self::translate_and_speak(
                            &config,
                            committed.commit_id,
                            &committed.text,
                            &session_id,
                            &event_tx,
                        )
                        .await;
                    }

                    let _ = event_tx
                        .send(ServerMessage::TurnComplete {
                            session_id: session_id.clone(),
                        })
                        .await;
                }

                SttEvent::Error { message } => {
                    let _ = event_tx
                        .send(ServerMessage::Error {
                            session_id: session_id.clone(),
                            code: "DEEPGRAM_STT_ERROR".into(),
                            message,
                        })
                        .await;
                }

                SttEvent::Closed => break,
            }
        }

        // Session ended — flush remaining
        let mut seg = segmentation.lock().await;
        let flushed = seg.flush_all();
        let total = seg.committed_segments().len() as u64;
        drop(seg);

        if let Some(committed) = flushed {
            let _ = event_tx
                .send(ServerMessage::CommitSrc {
                    session_id: session_id.clone(),
                    commit_id: committed.commit_id,
                    text: committed.text.clone(),
                })
                .await;

            Self::translate_and_speak(
                &config,
                committed.commit_id,
                &committed.text,
                &session_id,
                &event_tx,
            )
            .await;
        }

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
        config: Arc<TypecastInterpConfig>,
    ) {
        let tick = tokio::time::Duration::from_millis(100);
        let mut interval = tokio::time::interval(tick);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut seg = segmentation.lock().await;
                    let mut commits = Vec::new();
                    while let Some(committed) = seg.try_commit() {
                        let _ = event_tx.send(ServerMessage::CommitSrc {
                            session_id: session_id.clone(),
                            commit_id: committed.commit_id,
                            text: committed.text.clone(),
                        }).await;
                        commits.push(committed);
                    }
                    drop(seg);

                    for committed in commits {
                        Self::translate_and_speak(
                            &config,
                            committed.commit_id,
                            &committed.text,
                            &session_id,
                            &event_tx,
                        ).await;
                    }
                }
                _ = stop_rx.recv() => {
                    tracing::debug!(session_id = %session_id, "Tick timer stopped (Typecast interp)");
                    break;
                }
            }
        }
    }
}
