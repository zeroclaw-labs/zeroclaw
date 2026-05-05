//! STT+LLM+TTS interpretation session (Typecast pipeline).
//!
//! Combines on-device Gemma 4 STT with LLM-based translation and
//! Typecast TTS to produce interpreted speech in the user's cloned
//! voice (or a best-matching fallback voice).
//!
//! Until 2026-05 this pipeline used Deepgram for STT. Gemma 4 (E2B/E4B
//! audio-capable tier) replaced it because (a) Gemma is free / on-device
//! and (b) everyday interpretation speech is well within its accuracy
//! envelope. The cost/latency profile is now: zero STT cost + ~1.5–3 s
//! per utterance (Gemma is request/response, not streaming) + LLM
//! translation + Typecast TTS.
//!
//! ## Architecture
//!
//! ```text
//! Client mic ─▸ audio_chunk ─▸ TypecastInterpSession
//!                                    │
//!                       Gemma 4 STT (on-device, Ollama)
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
//!    user's gender, age, and target language. Resolved by the gateway
//!    via [`select_fallback_voice_id`] before session start.

use base64::Engine;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::deepgram_stt::SttEvent;
use super::events::ServerMessage;
use super::gemma_asr::{GemmaAsrConfig, GemmaAsrSession, DEFAULT_OLLAMA_URL};
use super::pipeline::{LanguageCode, VoiceAge, VoiceGender};
use super::simul::{SegmentationConfig, SegmentationEngine};
use super::voice_chat_pipeline::{QueryRoute, SttResult, VoiceChatPipeline};
use super::voice_messages::{
    ask_user_to_repeat, confirm_interpretation_fallback, confirm_interpretation_prefix,
};

// ── Configuration ─────────────────────────────────────────────────

/// Configuration for a Typecast-based interpretation session.
#[derive(Debug, Clone)]
pub struct TypecastInterpConfig {
    /// Unique session identifier.
    pub session_id: String,
    /// Ollama base URL for Gemma STT (no trailing slash). Defaults
    /// to `http://127.0.0.1:11434` when empty.
    pub gemma_base_url: String,
    /// Gemma 4 audio-capable model tag (e.g. `"gemma4:e4b"` /
    /// `"gemma4:e2b"`). The gateway resolves the right tier per
    /// device RAM via `LocalLlmConfig::default_model` and passes it
    /// here; `gemma_asr` itself does not pick a tier.
    pub gemma_model: String,
    /// Source language code (what the user speaks). Passed to Gemma
    /// as a transcription hint.
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
    /// Set by the gateway via [`select_fallback_voice_id`] before
    /// session start. Required when `voice_clone_id` is None;
    /// otherwise TTS will fail with an empty voice id.
    pub fallback_voice_id: Option<String>,
    /// LLM API key for translation (Gemini / OpenAI / etc.).
    pub llm_api_key: String,
    /// LLM model name (e.g. "gemini-3.1-flash-lite-preview").
    pub llm_model: String,
    /// LLM API base URL.
    pub llm_base_url: String,
    /// Segmentation configuration. Applied to Gemma `Final` events
    /// the same way it was applied to Deepgram finals: each finalized
    /// utterance is appended to the segmenter and committed by
    /// length / silence rules.
    pub segmentation: SegmentationConfig,
    /// Bidirectional mode.
    pub bidirectional: bool,
    /// Optional voice-chat self-validation pipeline. When present,
    /// each committed Gemma transcript runs through Gemma 4's
    /// JSON-only self-check before being sent to LLM translation.
    /// On `AskUserToRepeat` / `ConfirmInterpretation` routes the
    /// pipeline emits a re-ask in the speaker's own language and
    /// SKIPS translation for that segment — saving the cloud LLM
    /// call entirely when Gemma was unsure of the input. On
    /// `SimpleGemma` and `ComplexLlm` routes the pipeline proceeds
    /// to translation as before. When `None`, behavior is identical
    /// to pre-PR-C: every committed segment is translated.
    ///
    /// The session uses this only for `validate_only(...)`; the
    /// pipeline's `validate_and_answer` path (which would call its
    /// own LLM for direct answers) is not used in interpretation
    /// because the interpretation context wants translation, not a
    /// conversational answer.
    pub voice_validation: Option<Arc<VoiceChatPipeline>>,
}

// ── Voice matching ────────────────────────────────────────────────

/// Map LanguageCode to Typecast ISO 639-3 code.
///
/// Typecast does not actually support 75 languages today — its ssfm-v30
/// catalog covers ~37 (per `handle_api_voices_list` in `gateway/api.rs`).
/// For language codes outside Typecast's catalog we still return a
/// best-effort ISO 639-3 string; the Typecast TTS call will fail
/// gracefully and the gateway will surface a `NO_TYPECAST_VOICE` error
/// (see `select_fallback_voice_id`). That is preferable to refusing to
/// build the request at all — it lets the rest of the voice pipeline
/// compile against the full enum, and the only user-visible
/// consequence is a clear error instead of a silent fallback.
pub fn lang_to_typecast_iso3(lang: LanguageCode) -> &'static str {
    match lang {
        // East Asia
        LanguageCode::Ko => "kor",
        LanguageCode::Ja => "jpn",
        LanguageCode::Zh | LanguageCode::ZhTw => "cmn",
        LanguageCode::Mn => "mon",
        // Southeast Asia
        LanguageCode::Vi => "vie",
        LanguageCode::Th => "tha",
        LanguageCode::Id => "ind",
        LanguageCode::Ms => "msa",
        LanguageCode::Tl => "fil",
        LanguageCode::My => "mya",
        LanguageCode::Km => "khm",
        LanguageCode::Lo => "lao",
        // South Asia
        LanguageCode::Hi => "hin",
        LanguageCode::Bn => "ben",
        LanguageCode::Ta => "tam",
        LanguageCode::Te => "tel",
        LanguageCode::Mr => "mar",
        LanguageCode::Gu => "guj",
        LanguageCode::Kn => "kan",
        LanguageCode::Ml => "mal",
        LanguageCode::Pa => "pan",
        LanguageCode::Or => "ori",
        LanguageCode::Si => "sin",
        LanguageCode::Ur => "urd",
        LanguageCode::Ne => "nep",
        LanguageCode::Sd => "snd",
        // Europe — Western Latin
        LanguageCode::En => "eng",
        LanguageCode::Es => "spa",
        LanguageCode::Fr => "fra",
        LanguageCode::De => "deu",
        LanguageCode::It => "ita",
        LanguageCode::Pt => "por",
        LanguageCode::Nl => "nld",
        LanguageCode::Pl => "pol",
        LanguageCode::Cs => "ces",
        LanguageCode::Sv => "swe",
        LanguageCode::Da => "dan",
        LanguageCode::No => "nor",
        LanguageCode::Fi => "fin",
        LanguageCode::Is => "isl",
        LanguageCode::Ga => "gle",
        LanguageCode::Cy => "cym",
        LanguageCode::Mt => "mlt",
        LanguageCode::Eu => "eus",
        LanguageCode::Ca => "cat",
        LanguageCode::Gl => "glg",
        // Europe — Central / Southeastern Latin
        LanguageCode::Hu => "hun",
        LanguageCode::Ro => "ron",
        LanguageCode::Sk => "slk",
        LanguageCode::Sl => "slv",
        LanguageCode::Hr => "hrv",
        LanguageCode::Sr => "srp",
        LanguageCode::Bs => "bos",
        LanguageCode::Sq => "sqi",
        LanguageCode::Et => "est",
        LanguageCode::Lv => "lav",
        LanguageCode::Lt => "lit",
        // Europe — East Slavic Cyrillic
        LanguageCode::Ru => "rus",
        LanguageCode::Uk => "ukr",
        LanguageCode::Be => "bel",
        LanguageCode::Bg => "bul",
        LanguageCode::Mk => "mkd",
        // Europe — other scripts
        LanguageCode::El => "ell",
        LanguageCode::Hy => "hye",
        LanguageCode::Ka => "kat",
        LanguageCode::Tr => "tur",
        // Middle East
        LanguageCode::Ar => "ara",
        LanguageCode::He => "heb",
        LanguageCode::Fa => "fas",
        // Central Asia
        LanguageCode::Kk => "kaz",
        LanguageCode::Uz => "uzb",
        LanguageCode::Az => "aze",
        // Africa
        LanguageCode::Sw => "swa",
        LanguageCode::Am => "amh",
        LanguageCode::Yo => "yor",
        LanguageCode::Ha => "hau",
        LanguageCode::Zu => "zul",
        LanguageCode::Af => "afr",
        LanguageCode::So => "som",
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

// ── Fallback voice resolution ────────────────────────────────────

/// Pick the best Typecast `voice_id` for a user when they have NOT
/// supplied a clone — fetches `/v2/voices` (filtered by gender +
/// target language, model `ssfm-v30`) and picks the highest-scoring
/// candidate per [`voice_match_score`].
///
/// The gateway calls this once per session before constructing
/// [`TypecastInterpConfig`], so the interp loop never hits the
/// "no voice id → silent TTS" branch the previous TODO described.
///
/// Returns `Ok(Some(voice_id))` on a real match, `Ok(None)` when the
/// catalog has no matching voices (caller should surface a clear
/// error to the client), `Err` on transport / parse failure.
pub async fn select_fallback_voice_id(
    typecast_api_key: &str,
    user_gender: VoiceGender,
    user_age: VoiceAge,
    target_lang: LanguageCode,
) -> anyhow::Result<Option<String>> {
    if typecast_api_key.is_empty() {
        anyhow::bail!("Typecast API key not configured");
    }

    let gender_param = match user_gender {
        VoiceGender::Male => "male",
        VoiceGender::Female => "female",
    };
    let lang_iso3 = lang_to_typecast_iso3(target_lang);

    // Pull a generous window per gender+language. The catalog is
    // small enough (<1000 voices) that paging is unnecessary.
    let url = format!(
        "https://api.typecast.ai/v2/voices?model=ssfm-v30&gender={gender_param}&language={lang_iso3}"
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("X-API-KEY", typecast_api_key)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Typecast /v2/voices error {status}: {body}");
    }

    let payload: serde_json::Value = resp.json().await?;
    let voices = payload
        .get("voices")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut best: Option<(u32, String)> = None;
    for voice in voices.iter() {
        let voice_id = match voice.get("voice_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => continue,
        };
        let voice_gender = voice.get("gender").and_then(|v| v.as_str()).unwrap_or("");
        let voice_age = voice.get("age").and_then(|v| v.as_str()).unwrap_or("");
        let use_cases: Vec<String> = voice
            .get("use_cases")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let score = voice_match_score(voice_gender, voice_age, &use_cases, user_gender, user_age);
        if score == 0 {
            continue;
        }
        match &best {
            Some((best_score, _)) if *best_score >= score => {}
            _ => best = Some((score, voice_id.to_string())),
        }
    }

    Ok(best.map(|(_, id)| id))
}

// ── TTS call ──────────────────────────────────────────────────────

/// Public wrapper around `typecast_tts_synthesize` so the new
/// Rust-native voice-chat session (`voice/voice_chat_session.rs`)
/// can call Typecast through the same code path without
/// duplicating the request shape.
pub async fn call_typecast_tts_synthesize(
    api_key: &str,
    voice_id: &str,
    text: &str,
    language: &str,
    voice_clone_id: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    typecast_tts_synthesize(api_key, voice_id, text, language, voice_clone_id).await
}

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

        // Build Gemma ASR config — model tier is whatever the gateway
        // resolved for this device. Pass the source language as a
        // transcription hint so Gemma stays on script.
        let gemma_base_url = if config.gemma_base_url.trim().is_empty() {
            DEFAULT_OLLAMA_URL.to_string()
        } else {
            config.gemma_base_url.clone()
        };
        let asr_config = GemmaAsrConfig {
            session_id: session_id.clone(),
            base_url: gemma_base_url,
            model: config.gemma_model.clone(),
            language_hint: Some(config.source_lang.as_str().to_string()),
            ..Default::default()
        };

        tracing::info!(
            session_id = %session_id,
            source = config.source_lang.as_str(),
            target = config.target_lang.as_str(),
            gemma_model = %config.gemma_model,
            has_voice_clone = config.voice_clone_id.is_some(),
            has_fallback_voice = config.fallback_voice_id.is_some(),
            "Starting Typecast interpretation session (Gemma STT + LLM + Typecast TTS)"
        );

        let asr_session = GemmaAsrSession::start(asr_config).await?;

        let (audio_tx, audio_rx) = mpsc::channel::<Vec<u8>>(512);
        let (event_tx, event_rx) = mpsc::channel::<ServerMessage>(256);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);

        let segmentation = Arc::new(Mutex::new(SegmentationEngine::new(config.segmentation.clone())));
        let asr_session = Arc::new(asr_session);
        let config = Arc::new(config);

        // Spawn audio forwarder
        let asr_for_audio = Arc::clone(&asr_session);
        tokio::spawn(async move {
            Self::audio_forwarder(audio_rx, asr_for_audio).await;
        });

        // Spawn event processor (STT → segmentation → LLM → TTS)
        let asr_for_events = Arc::clone(&asr_session);
        let seg_for_events = Arc::clone(&segmentation);
        let event_tx_events = event_tx.clone();
        let sid_events = session_id.clone();
        let cfg_events = Arc::clone(&config);
        tokio::spawn(async move {
            Self::event_processor(
                asr_for_events,
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
        asr: Arc<GemmaAsrSession>,
    ) {
        while let Some(pcm) = audio_rx.recv().await {
            if let Err(e) = asr.send_audio(pcm).await {
                tracing::warn!(error = %e, "Failed to forward audio to Gemma ASR");
                break;
            }
        }
        // Gemma ASR has no finalize() — closing the audio channel
        // simply lets its background loop drain and exit.
        asr.close().await;
    }

    // ── Internal: self-validation gate ─────────────────────────────

    /// Validate a committed source segment via the optional
    /// `voice_chat_pipeline` self-check, then either:
    ///   * translate it normally (SimpleGemma / ComplexLlm route, or
    ///     when no validator is configured), or
    ///   * emit a re-ask in the speaker's own language and SKIP
    ///     translation (AskUserToRepeat / ConfirmInterpretation route).
    ///
    /// `voice_retry_counts` is the per-speaker counter the
    /// `event_processor` keeps for the staircase.
    async fn validate_then_translate(
        config: &TypecastInterpConfig,
        commit_id: u64,
        source_text: &str,
        session_id: &str,
        event_tx: &mpsc::Sender<ServerMessage>,
        voice_retry_counts: &mut HashMap<LanguageCode, u8>,
    ) {
        // No validator configured → preserve pre-PR-C behavior:
        // translate every committed segment unconditionally.
        let Some(validator) = config.voice_validation.as_ref() else {
            Self::translate_and_speak(config, commit_id, source_text, session_id, event_tx).await;
            return;
        };

        // Run the JSON-only Gemma self-check. Compose the StttResult
        // with the gateway's source language as the detection
        // default — see `SttResult::default_language` doc comment.
        // We don't have a real STT confidence here (Gemma ASR
        // doesn't surface one to typecast_interp); pass 1.0 as a
        // neutral high-confidence value so the validation prompt's
        // own confidence call drives the decision instead of the
        // STT confidence threshold.
        //
        // For retry_count we look up by the language we *expect* the
        // speaker to be using. We don't yet know the detected
        // language — that comes from validation itself. So we
        // pessimistically use the source-lang default; if validation
        // detects a different language we'll bump that bucket on
        // re-ask routes. This is a small inaccuracy on the very
        // first turn but self-corrects within one round trip.
        let default_lang = config.source_lang;
        let pre_retry_count = voice_retry_counts
            .get(&default_lang)
            .copied()
            .unwrap_or(0);
        let stt = SttResult {
            text: source_text.to_string(),
            confidence: 1.0,
            processing_time_ms: 0,
            voice_retry_count: pre_retry_count,
            default_language: Some(default_lang),
        };

        let validation = match validator.validate_only(&stt).await {
            Ok(v) => v,
            Err(e) => {
                // Validation failure must NEVER block the
                // interpretation flow — the user is having a real
                // conversation. Log and translate as if validation
                // never happened.
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "voice-validation failed; proceeding with unvalidated translation"
                );
                Self::translate_and_speak(config, commit_id, source_text, session_id, event_tx)
                    .await;
                return;
            }
        };

        match validation.route {
            QueryRoute::SimpleGemma | QueryRoute::ComplexLlm => {
                // Gemma is sure it understood. Reset this speaker's
                // retry counter (the conversation has progressed)
                // and translate normally.
                voice_retry_counts.insert(validation.detected_language, 0);
                Self::translate_and_speak(config, commit_id, source_text, session_id, event_tx)
                    .await;
            }
            QueryRoute::AskUserToRepeat => {
                // Bump the speaker's retry counter so the next
                // utterance from the same language advances to
                // ConfirmInterpretation.
                let entry = voice_retry_counts
                    .entry(validation.detected_language)
                    .or_insert(0);
                *entry = entry.saturating_add(1);

                let phrase = ask_user_to_repeat(validation.detected_language).to_string();
                tracing::debug!(
                    session_id = %session_id,
                    lang = validation.detected_language.as_str(),
                    "voice-validation: AskUserToRepeat (translation skipped)"
                );
                // Send as CommitTgt so the existing client UI surfaces
                // it on the translated-text rail. We pair it with a
                // TTS synthesis in the speaker's own language so the
                // user actually hears the re-ask out loud.
                let _ = event_tx
                    .send(ServerMessage::CommitTgt {
                        session_id: session_id.to_string(),
                        commit_id,
                        text: phrase.clone(),
                    })
                    .await;
                Self::synthesize_phrase(
                    config,
                    commit_id,
                    &phrase,
                    validation.detected_language,
                    session_id,
                    event_tx,
                )
                .await;
            }
            QueryRoute::ConfirmInterpretation => {
                let entry = voice_retry_counts
                    .entry(validation.detected_language)
                    .or_insert(0);
                *entry = entry.saturating_add(1);

                let paraphrase = validation.interpreted_meaning.trim();
                let phrase = if paraphrase.is_empty() {
                    confirm_interpretation_fallback(validation.detected_language).to_string()
                } else {
                    format!(
                        "{prefix} '{paraphrase}'",
                        prefix = confirm_interpretation_prefix(validation.detected_language)
                    )
                };
                tracing::debug!(
                    session_id = %session_id,
                    lang = validation.detected_language.as_str(),
                    "voice-validation: ConfirmInterpretation (translation skipped)"
                );
                let _ = event_tx
                    .send(ServerMessage::CommitTgt {
                        session_id: session_id.to_string(),
                        commit_id,
                        text: phrase.clone(),
                    })
                    .await;
                Self::synthesize_phrase(
                    config,
                    commit_id,
                    &phrase,
                    validation.detected_language,
                    session_id,
                    event_tx,
                )
                .await;
            }
        }
    }

    /// TTS-synthesize a fixed phrase (re-ask / confirm message) in
    /// the speaker's own language. Used by `validate_then_translate`
    /// on the re-ask routes — separate from `translate_and_speak`
    /// which couples translation + TTS, so the re-ask path can skip
    /// the LLM call entirely while still producing audible output.
    async fn synthesize_phrase(
        config: &TypecastInterpConfig,
        _commit_id: u64,
        phrase: &str,
        lang: LanguageCode,
        session_id: &str,
        event_tx: &mpsc::Sender<ServerMessage>,
    ) {
        let voice_id = match config
            .voice_clone_id
            .as_deref()
            .or(config.fallback_voice_id.as_deref())
        {
            Some(id) if !id.is_empty() => id,
            _ => {
                tracing::warn!(
                    session_id = %session_id,
                    "no voice id available for re-ask TTS; phrase delivered as text only"
                );
                return;
            }
        };

        let lang_iso3 = lang_to_typecast_iso3(lang);
        match typecast_tts_synthesize(
            &config.typecast_api_key,
            voice_id,
            phrase,
            lang_iso3,
            config.voice_clone_id.as_deref(),
        )
        .await
        {
            Ok(pcm_data) => {
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
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "re-ask TTS synthesis failed; phrase delivered as text only"
                );
            }
        }
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

        // 2. Typecast TTS synthesis. The gateway is supposed to have
        // resolved a fallback_voice_id via select_fallback_voice_id()
        // before construction; if both clone + fallback are missing,
        // surface a real error instead of silently skipping audio.
        let voice_id = match config
            .voice_clone_id
            .as_deref()
            .or(config.fallback_voice_id.as_deref())
        {
            Some(id) if !id.is_empty() => id,
            _ => {
                tracing::error!(
                    session_id = %session_id,
                    "No Typecast voice id available (no clone, no fallback resolved)"
                );
                let _ = event_tx
                    .send(ServerMessage::Error {
                        session_id: session_id.to_string(),
                        code: "TTS_NO_VOICE_ID".into(),
                        message: "No Typecast voice available for this user / target language. \
                                  Please pick a voice in Settings or use voice cloning."
                            .to_string(),
                    })
                    .await;
                return;
            }
        };

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
        asr: Arc<GemmaAsrSession>,
        segmentation: Arc<Mutex<SegmentationEngine>>,
        event_tx: mpsc::Sender<ServerMessage>,
        session_id: String,
        config: Arc<TypecastInterpConfig>,
    ) {
        // Per-speaker voice-retry counter for the self-validation
        // staircase. Bidirectional interpretation flips speakers
        // turn-by-turn (Korean speaker → English speaker → …), so a
        // single global counter would mis-attribute "this is your
        // second voice retry" across the language boundary. Keying
        // by detected `LanguageCode` keeps each speaker's staircase
        // independent.
        //
        // The counter is pure local state — no need for an Arc or
        // Mutex because the entire `event_processor` runs in one
        // tokio task. It resets to 0 for a given language as soon
        // as a successful translation happens for that language.
        let mut voice_retry_counts: HashMap<LanguageCode, u8> = HashMap::new();

        let mut rx = asr.event_rx.lock().await;
        loop {
            let event = match rx.recv().await {
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

                    // Validate (when configured) then translate + TTS
                    // each committed segment.
                    for committed in commits {
                        Self::validate_then_translate(
                            &config,
                            committed.commit_id,
                            &committed.text,
                            &session_id,
                            &event_tx,
                            &mut voice_retry_counts,
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
                            code: "GEMMA_STT_ERROR".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_match_score_zero_on_gender_mismatch() {
        // Female user, male voice → hard reject regardless of other fields.
        let s = voice_match_score(
            "male",
            "young_adult",
            &["Conversational".to_string()],
            VoiceGender::Female,
            VoiceAge::YoungAdult,
        );
        assert_eq!(s, 0);
    }

    #[test]
    fn voice_match_score_prefers_conversational_over_announcer() {
        let conv = voice_match_score(
            "female",
            "young_adult",
            &["Conversational".to_string()],
            VoiceGender::Female,
            VoiceAge::YoungAdult,
        );
        let news = voice_match_score(
            "female",
            "young_adult",
            &["News Reporter".to_string()],
            VoiceGender::Female,
            VoiceAge::YoungAdult,
        );
        assert!(
            conv > news,
            "Conversational should outrank News Reporter for interpretation; \
             conv={conv} news={news}"
        );
    }

    #[test]
    fn voice_match_score_age_exact_beats_age_off_by_one() {
        let exact = voice_match_score(
            "female",
            "middle_age",
            &["Conversational".to_string()],
            VoiceGender::Female,
            VoiceAge::MiddleAge,
        );
        let off_by_one = voice_match_score(
            "female",
            "young_adult",
            &["Conversational".to_string()],
            VoiceGender::Female,
            VoiceAge::MiddleAge,
        );
        assert!(
            exact > off_by_one,
            "Exact age match should outrank off-by-one; exact={exact} off={off_by_one}"
        );
    }

    #[test]
    fn voice_match_score_excludes_game_only_voice() {
        // Voice whose ONLY use case is "Game" → hard exclude.
        let s = voice_match_score(
            "female",
            "young_adult",
            &["Game".to_string()],
            VoiceGender::Female,
            VoiceAge::YoungAdult,
        );
        assert_eq!(s, 0);
    }

    #[test]
    fn lang_to_typecast_iso3_covers_supported_codes() {
        // Spot-check a few to make sure no panic on the common path.
        assert_eq!(lang_to_typecast_iso3(LanguageCode::Ko), "kor");
        assert_eq!(lang_to_typecast_iso3(LanguageCode::En), "eng");
        assert_eq!(lang_to_typecast_iso3(LanguageCode::Ja), "jpn");
        assert_eq!(lang_to_typecast_iso3(LanguageCode::Zh), "cmn");
        assert_eq!(lang_to_typecast_iso3(LanguageCode::ZhTw), "cmn");
    }

    // ── PR-C: per-speaker retry counter behavior ──────────────────
    //
    // The full `validate_then_translate` integration touches an
    // mpsc::Sender, an async Provider trait object, and a Typecast
    // HTTP call — none of which a unit test can exercise. So we
    // pin the *retry-counter accounting* itself with a small
    // simulation that mirrors the relevant arms of the function.

    /// Mirror of the retry-bookkeeping in `validate_then_translate`.
    /// Kept in tests because it has no callers in production code —
    /// production calls the inline statements. If `validate_then_translate`'s
    /// counter logic changes, this helper must change in lockstep.
    fn simulate_route(
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
    fn retry_counter_increments_on_uncertain_then_resets_on_success() {
        let mut counts: HashMap<LanguageCode, u8> = HashMap::new();
        // First Korean utterance: uncertain → AskUserToRepeat
        simulate_route(&mut counts, QueryRoute::AskUserToRepeat, LanguageCode::Ko);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(1));
        // User re-speaks Korean: still uncertain → ConfirmInterpretation
        simulate_route(
            &mut counts,
            QueryRoute::ConfirmInterpretation,
            LanguageCode::Ko,
        );
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(2));
        // User confirms; pipeline routes to ComplexLlm → counter resets
        simulate_route(&mut counts, QueryRoute::ComplexLlm, LanguageCode::Ko);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(0));
    }

    #[test]
    fn retry_counter_is_independent_per_speaker_language() {
        // Bidirectional case: Korean speaker has trouble; English
        // speaker on the next turn comes through clean. The Korean
        // counter must NOT bleed into English.
        let mut counts: HashMap<LanguageCode, u8> = HashMap::new();
        simulate_route(&mut counts, QueryRoute::AskUserToRepeat, LanguageCode::Ko);
        simulate_route(&mut counts, QueryRoute::ComplexLlm, LanguageCode::En);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(1));
        assert_eq!(counts.get(&LanguageCode::En).copied(), Some(0));
        // Now the English speaker has a noisy turn — independently
        // bumps the English counter, leaves Korean untouched.
        simulate_route(&mut counts, QueryRoute::AskUserToRepeat, LanguageCode::En);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(1));
        assert_eq!(counts.get(&LanguageCode::En).copied(), Some(1));
    }

    #[test]
    fn retry_counter_saturates_does_not_overflow() {
        let mut counts: HashMap<LanguageCode, u8> = HashMap::new();
        counts.insert(LanguageCode::Ko, u8::MAX);
        // Should saturate at u8::MAX, not panic.
        simulate_route(&mut counts, QueryRoute::AskUserToRepeat, LanguageCode::Ko);
        assert_eq!(counts.get(&LanguageCode::Ko).copied(), Some(u8::MAX));
    }
}
