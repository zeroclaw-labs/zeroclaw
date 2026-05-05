//! Voice-chat HTTP endpoints — Rust-native replacements for the
//! pieces of the LiveKit-based voice-chat path that need to live on
//! the server side.
//!
//! ## Endpoints
//!
//! * `POST /api/voice/transcribe` — runs Gemma 4 ASR on a single
//!   uploaded audio buffer and returns the self-validation routing
//!   decision (transcript + route + speaker-language re-ask message
//!   if Gemma was uncertain). Used by clients that want HTTP-style
//!   one-shot voice-to-text conversion with the staircase UX.
//!
//! * `POST /api/billing/voice-usage` — credits the user's account
//!   for one finished voice turn. Until this endpoint existed the
//!   Python LiveKit agent was POSTing here and getting 404, which
//!   meant operator-key voice usage was effectively free for the
//!   user (and a quiet loss for the operator). The Python agent is
//!   on its way out (it will be replaced by a Rust-native WS path
//!   in a follow-up PR), but until that lands this endpoint plugs
//!   the billing leak.
//!
//! Neither endpoint is wired to a UI yet — they are deliberately
//! shipped as inert building blocks so the larger UX migration
//! (LiveKit → native WebSocket) can land in small reviewable
//! steps. The Tauri client will start calling them in a later PR.
//!
//! ## Auth and billing
//!
//! Both endpoints require a Bearer session token (same pattern as
//! `/api/llm/proxy`). Voice-usage credit deduction follows the
//! existing 2.2× operator-key multiplier defined in
//! `crate::billing::llm_router::OPERATOR_KEY_CREDIT_MULTIPLIER`.
//! The `LiveKitConfig.credit_multiplier` config field, which until
//! now was declarative-only, is finally honored when computing
//! voice-usage credits.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::voice::gemma_asr::{wrap_pcm16_in_wav, INPUT_SAMPLE_RATE};

// ── Shared helpers ────────────────────────────────────────────────

/// Extract `Bearer <token>` from the Authorization header.
fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

/// Validate the bearer token against the auth store and return the
/// authenticated `user_id`. Falls through to the pairing guard
/// (legacy single-device installs) when no auth_store is configured.
fn authenticate_user(state: &AppState, token: &str) -> Option<String> {
    if let Some(auth_store) = &state.auth_store {
        if let Some(session) = auth_store.validate_session(token) {
            return Some(session.user_id);
        }
    }
    if state.pairing.is_paired() && state.pairing.is_authenticated(token) {
        return Some("zeroclaw_local_operator".to_string());
    }
    None
}

// ── /api/voice/transcribe ────────────────────────────────────────

/// Request body for `POST /api/voice/transcribe`.
///
/// Audio is base64-encoded PCM16LE at 16 kHz mono — same format the
/// existing `/ws/voice` handler accepts in `audio_chunk` envelopes.
/// Wrapping it in JSON (rather than multipart) keeps the schema
/// trivial; even a 30-second utterance fits comfortably under the
/// chat body limit (`CHAT_MAX_BODY_SIZE`).
#[derive(Debug, Deserialize)]
pub struct VoiceTranscribeRequest {
    /// Base64-encoded raw PCM16LE samples. 16 kHz, mono, no header.
    pub pcm16le: String,
    /// Speaker's language hint (BCP-47, e.g. `"ko"`, `"en"`). Acts
    /// as the fallback for `voice_chat_pipeline` script detection
    /// when the utterance is too short to resolve. Optional.
    #[serde(default)]
    pub source_language: Option<String>,
    /// How many voice re-attempts have happened on this conversational
    /// turn already (the staircase counter). `0` for a fresh turn.
    #[serde(default)]
    pub voice_retry_count: u8,
    /// Optional Gemma model override (e.g. `"gemma4:e4b"`). When
    /// absent the server uses the device's `LocalLlmConfig` choice.
    #[serde(default)]
    pub gemma_model: Option<String>,
}

/// Response body for `POST /api/voice/transcribe`.
#[derive(Debug, Serialize)]
pub struct VoiceTranscribeResponse {
    /// The Gemma-produced transcript. Empty when ASR returned
    /// nothing (silence / inaudible input).
    pub stt_text: String,
    /// The script-detected speaker language (BCP-47).
    pub detected_language: String,
    /// Routing decision the self-validation pipeline reached. One of:
    /// `"simple_gemma"`, `"ask_user_to_repeat"`,
    /// `"confirm_interpretation"`, `"complex_llm"`.
    pub route: String,
    /// When the route is a re-ask (`AskUserToRepeat` /
    /// `ConfirmInterpretation`), this is the localized phrase the
    /// caller should TTS back to the user. `None` for the other
    /// routes; the caller should proceed to LLM-based answer
    /// generation in those cases.
    pub response_message: Option<String>,
    /// What the staircase counter should be on the next voice turn
    /// from this speaker. The caller is responsible for echoing
    /// this back in the `voice_retry_count` field of the next
    /// transcribe request.
    pub voice_retry_count_next: u8,
}

/// Handler for `POST /api/voice/transcribe`.
pub async fn handle_voice_transcribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VoiceTranscribeRequest>,
) -> impl IntoResponse {
    use base64::Engine;

    // 1. Auth
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Missing or invalid Authorization header"})),
            )
                .into_response();
        }
    };
    let _user_id = match authenticate_user(&state, &token) {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Invalid or expired session token"})),
            )
                .into_response();
        }
    };

    // 2. Decode the base64 audio. Reject empties early.
    let pcm = match base64::engine::general_purpose::STANDARD.decode(&req.pcm16le) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid base64 audio: {e}")})),
            )
                .into_response();
        }
    };
    if pcm.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Empty audio payload"})),
        )
            .into_response();
    }

    // 3. Resolve Gemma endpoint + model. Honor explicit request
    //    override; otherwise fall back to env var; otherwise the
    //    persisted device-tier default.
    let base_url = std::env::var("OLLAMA_BASE_URL")
        .unwrap_or_else(|_| crate::voice::gemma_asr::DEFAULT_OLLAMA_URL.to_string());
    let model = if let Some(m) = req.gemma_model.as_deref().filter(|s| !s.is_empty()) {
        m.to_string()
    } else if let Ok(env_model) = std::env::var("GEMMA_ASR_MODEL") {
        if env_model.trim().is_empty() {
            resolve_default_gemma_model().await
        } else {
            env_model
        }
    } else {
        resolve_default_gemma_model().await
    };

    // 4. Run Gemma ASR via a one-shot synchronous wrapper. We do
    //    NOT spin up a `GemmaAsrSession` (which is built for
    //    streaming PCM and emits SttEvents over time) — for a
    //    single uploaded buffer we go straight to the underlying
    //    Ollama chat call so we get one response per request.
    let stt_text = match transcribe_oneshot(&base_url, &model, &pcm, req.source_language.as_deref()).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Gemma ASR failed: {e}")})),
            )
                .into_response();
        }
    };

    // 5. Run self-validation. If we cannot construct the validator
    //    (Ollama unreachable, etc.) we still return the transcript —
    //    the caller can decide what to do with un-validated text.
    let default_lang = req
        .source_language
        .as_deref()
        .and_then(crate::voice::pipeline::LanguageCode::from_str_code);

    let validator = build_voice_chat_validator(&base_url, &model);

    let stt_result = crate::voice::voice_chat_pipeline::SttResult {
        text: stt_text.clone(),
        confidence: 1.0,
        processing_time_ms: 0,
        voice_retry_count: req.voice_retry_count,
        default_language: default_lang,
    };

    let (route, response_message, detected_lang, retry_next) = match validator
        .as_ref()
        .map(|v| v.validate_only(&stt_result))
    {
        Some(fut) => match fut.await {
            Ok(v) => render_validation_outcome(&v, req.voice_retry_count),
            Err(e) => {
                tracing::warn!(error = %e, "voice transcribe: self-validation failed; returning unvalidated transcript");
                (
                    "complex_llm".to_string(),
                    None,
                    default_lang
                        .unwrap_or(crate::voice::pipeline::LanguageCode::En)
                        .as_str()
                        .to_string(),
                    req.voice_retry_count,
                )
            }
        },
        None => (
            "complex_llm".to_string(),
            None,
            default_lang
                .unwrap_or(crate::voice::pipeline::LanguageCode::En)
                .as_str()
                .to_string(),
            req.voice_retry_count,
        ),
    };

    (
        StatusCode::OK,
        Json(VoiceTranscribeResponse {
            stt_text,
            detected_language: detected_lang,
            route,
            response_message,
            voice_retry_count_next: retry_next,
        }),
    )
        .into_response()
}

/// Build the optional validation pipeline used by transcribe.
/// Returns `None` (rather than `Err`) on construction failure so
/// the transcribe endpoint always returns the transcript even when
/// validation cannot run.
fn build_voice_chat_validator(
    gemma_base_url: &str,
    gemma_model: &str,
) -> Option<std::sync::Arc<crate::voice::voice_chat_pipeline::VoiceChatPipeline>> {
    use crate::providers::create_provider_with_url;
    use crate::voice::voice_chat_pipeline::VoiceChatPipeline;

    let gemma_provider = create_provider_with_url("ollama", None, Some(gemma_base_url)).ok()?;
    // The transcribe endpoint never reaches the ComplexLlm path
    // (it returns the routing decision to the caller, who then
    // does its own LLM call). So the `llm` provider here is
    // load-bearing for VoiceChatPipeline construction only and
    // can be a no-network ollama handle — same instance is fine.
    let llm_provider = create_provider_with_url("ollama", None, Some(gemma_base_url)).ok()?;
    Some(std::sync::Arc::new(VoiceChatPipeline::new(
        std::sync::Arc::from(gemma_provider),
        gemma_model,
        std::sync::Arc::from(llm_provider),
        gemma_model,
    )))
}

/// Map a `ValidationResult` into the wire-format tuple the
/// transcribe response expects: (route_name, response_message,
/// detected_language_code, next_retry_count).
fn render_validation_outcome(
    validation: &crate::voice::voice_chat_pipeline::ValidationResult,
    current_retry: u8,
) -> (String, Option<String>, String, u8) {
    use crate::voice::voice_chat_pipeline::QueryRoute;
    use crate::voice::voice_messages::{
        ask_user_to_repeat, confirm_interpretation_fallback, confirm_interpretation_prefix,
    };

    let lang = validation.detected_language;
    match validation.route {
        QueryRoute::SimpleGemma => (
            "simple_gemma".to_string(),
            None,
            lang.as_str().to_string(),
            0,
        ),
        QueryRoute::AskUserToRepeat => (
            "ask_user_to_repeat".to_string(),
            Some(ask_user_to_repeat(lang).to_string()),
            lang.as_str().to_string(),
            current_retry.saturating_add(1),
        ),
        QueryRoute::ConfirmInterpretation => {
            let paraphrase = validation.interpreted_meaning.trim();
            let phrase = if paraphrase.is_empty() {
                confirm_interpretation_fallback(lang).to_string()
            } else {
                format!(
                    "{prefix} '{paraphrase}'",
                    prefix = confirm_interpretation_prefix(lang)
                )
            };
            (
                "confirm_interpretation".to_string(),
                Some(phrase),
                lang.as_str().to_string(),
                current_retry.saturating_add(1),
            )
        }
        QueryRoute::ComplexLlm => (
            "complex_llm".to_string(),
            None,
            lang.as_str().to_string(),
            0,
        ),
    }
}

/// Look up the device-tier Gemma model the installer chose, falling
/// back to the conservative `gemma4:e4b` default.
async fn resolve_default_gemma_model() -> String {
    match crate::local_llm::LocalLlmConfig::default_path() {
        Ok(path) => match crate::local_llm::LocalLlmConfig::load(&path).await {
            Ok(cfg) => cfg.default_model,
            Err(_) => crate::voice::gemma_asr::DEFAULT_MODEL.to_string(),
        },
        Err(_) => crate::voice::gemma_asr::DEFAULT_MODEL.to_string(),
    }
}

/// One-shot Ollama chat call mirroring the streaming Gemma ASR
/// session's `flush_and_transcribe` path. Wraps PCM in WAV, base64s
/// it, posts to `/api/chat` under the `images` field (Ollama's quirky
/// multimodal channel — see `gemma_asr` doc comment), and returns the
/// model's transcript text.
async fn transcribe_oneshot(
    base_url: &str,
    model: &str,
    pcm: &[u8],
    language_hint: Option<&str>,
) -> anyhow::Result<String> {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;

    let wav = wrap_pcm16_in_wav(pcm, INPUT_SAMPLE_RATE, 1);
    let b64 = BASE64.encode(&wav);

    let prompt = match language_hint {
        Some(lang) if !lang.is_empty() => format!(
            "Transcribe this {lang} audio. Output ONLY the literal transcript text \
             with no additional commentary, no translation, no headers."
        ),
        _ => "Transcribe this audio. Output ONLY the literal transcript text \
              in the spoken language with no additional commentary."
            .to_string(),
    };

    let body = serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": prompt,
            "images": [b64],
        }],
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let resp = client
        .post(format!("{base_url}/api/chat"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned {status}: {err}");
    }
    let parsed: serde_json::Value = resp.json().await?;
    let text = parsed
        .pointer("/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(text)
}

// ── /api/billing/voice-usage ─────────────────────────────────────

/// Request body for `POST /api/billing/voice-usage`.
///
/// The fields mirror what the Python LiveKit agent's `billing_hook`
/// has been POSTing all along — see
/// `services/livekit-agents/billing_hook.py`. We accept a few extras
/// (`stt_audio_seconds`, `tts_chars`) that the agent reports today
/// but that the credit math doesn't yet use; tracking them in the
/// cost ledger gives us material for a later refinement of the
/// voice-cost estimate.
#[derive(Debug, Deserialize)]
pub struct VoiceUsageRequest {
    /// User-id the agent associates with this turn (LiveKit
    /// participant identity, which is the MoA user_id).
    pub user_id: String,
    /// Provider for the LLM step (e.g. `"gemini"`, `"anthropic"`).
    pub provider: String,
    /// Model used for the LLM step (e.g. `"gemini-3.1-flash-lite-preview"`).
    pub model: String,
    /// Tokens billed by the LLM provider for this turn.
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    /// TTS characters synthesized this turn. Recorded for future
    /// cost-estimate refinement; not yet a billing factor.
    #[serde(default)]
    pub tts_chars: i64,
    /// STT audio duration in seconds. Same status as `tts_chars`.
    #[serde(default)]
    pub stt_audio_seconds: f64,
    /// `true` when the user's own provider key was used (no
    /// operator burn → no credit deduction). Honors the same
    /// contract the LLM proxy uses for chat.
    #[serde(default)]
    pub using_user_key: bool,
}

#[derive(Debug, Serialize)]
pub struct VoiceUsageResponse {
    /// Credits actually deducted (`0` when `using_user_key=true` or
    /// when no PaymentManager is configured on this gateway).
    pub credits_deducted: u32,
    /// Remaining balance after the deduction (`None` when no
    /// PaymentManager is configured).
    pub balance_remaining: Option<u32>,
}

pub async fn handle_voice_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<VoiceUsageRequest>,
) -> impl IntoResponse {
    // 1. Auth — same Bearer-token check as transcribe.
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "Missing or invalid Authorization header"})),
            )
                .into_response();
        }
    };
    if authenticate_user(&state, &token).is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Invalid or expired session token"})),
        )
            .into_response();
    }

    // 2. Cost estimate. Reuse `CostTracker::estimate_cost` so the
    //    voice path uses the same per-model unit price as text chat.
    let cost_usd = crate::billing::tracker::CostTracker::estimate_cost(
        &req.model,
        req.input_tokens,
        req.output_tokens,
    );

    // 3. Always record in the cost tracker (even for user-key turns)
    //    so usage history is complete. Mirror the shape `llm_proxy`
    //    uses so voice and chat usage land in the same ledger format.
    if let Some(tracker) = &state.cost_tracker {
        // Negative token counts are nonsensical here — provider-reported
        // usage is always non-negative. `max(0) as u64` defensively
        // protects the ledger from a malformed agent payload, and
        // since `max(0)` rules out the sign-loss path the cast is
        // safe; we silence the lint locally rather than threading a
        // `try_into` chain that would never fail in practice.
        #[allow(clippy::cast_sign_loss)]
        let in_tok = req.input_tokens.max(0) as u64;
        #[allow(clippy::cast_sign_loss)]
        let out_tok = req.output_tokens.max(0) as u64;
        let usage = crate::cost::TokenUsage {
            model: req.model.clone(),
            input_tokens: in_tok,
            output_tokens: out_tok,
            total_tokens: in_tok.saturating_add(out_tok),
            cost_usd,
            timestamp: chrono::Utc::now(),
        };
        let _ = tracker.record_usage(usage);
    }

    // 4. User-key turns: no deduction. Return zero-credit success
    //    so the agent doesn't treat this as an error.
    if req.using_user_key {
        let balance = state
            .payment_manager
            .as_ref()
            .and_then(|pm| pm.lock().get_balance(&req.user_id).ok());
        return (
            StatusCode::OK,
            Json(VoiceUsageResponse {
                credits_deducted: 0,
                balance_remaining: balance,
            }),
        )
            .into_response();
    }

    // 5. Operator-key turns: apply the configured multiplier (the
    //    same `LiveKitConfig.credit_multiplier` field that until
    //    this PR was declarative-only) and deduct.
    let multiplier = {
        let cfg = state.config.lock();
        let m = cfg.livekit.credit_multiplier;
        // Defensive clamp — a misconfigured `0.0` or negative
        // multiplier would let the user use the operator's key for
        // free. Floor at the same 2.2× the LLM-router constant uses.
        if m <= 0.0 {
            2.2_f64
        } else {
            m
        }
    };

    // f64 -> u32: cost_usd is a small positive USD amount, multiplier
    // is a clamped positive scalar (>0.0), CREDITS_PER_USD is 1_000.
    // The product is bounded well under u32::MAX. `.ceil() as u32` is
    // a saturating cast in Rust 2021 so even an absurd input stays
    // bounded. `.max(1)` guarantees the user always pays at least
    // one credit per turn so a free-burn loophole is impossible.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let credits_to_deduct = ((cost_usd
        * multiplier
        * crate::billing::llm_router::CREDITS_PER_USD)
        .ceil() as u32)
        .max(1);

    let pm = match &state.payment_manager {
        Some(pm) => pm.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "Billing is not configured on this gateway",
                })),
            )
                .into_response();
        }
    };

    let deduction_result = {
        let pm = pm.lock();
        pm.deduct_credits(&req.user_id, credits_to_deduct)
    };

    match deduction_result {
        Ok(remaining) => {
            tracing::info!(
                user_id = %req.user_id,
                provider = %req.provider,
                model = %req.model,
                cost_usd,
                multiplier,
                credits_deducted = credits_to_deduct,
                "voice-usage: operator-key deduction recorded"
            );
            (
                StatusCode::OK,
                Json(VoiceUsageResponse {
                    credits_deducted: credits_to_deduct,
                    balance_remaining: Some(remaining),
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::warn!(
                user_id = %req.user_id,
                credits = credits_to_deduct,
                error = %e,
                "voice-usage: deduction failed (insufficient credits or DB error)"
            );
            (
                StatusCode::PAYMENT_REQUIRED,
                Json(serde_json::json!({
                    "error": format!("Credit deduction failed: {e}"),
                    "credits_required": credits_to_deduct,
                })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_outcome_simple_gemma_returns_zero_retry_no_message() {
        use crate::voice::pipeline::LanguageCode;
        use crate::voice::voice_chat_pipeline::{QueryRoute, ValidationResult};
        let v = ValidationResult {
            is_valid: true,
            confidence: 0.9,
            is_simple_query: true,
            route: QueryRoute::SimpleGemma,
            interpreted_meaning: String::new(),
            detected_language: LanguageCode::Ko,
            validation_time_ms: 1,
        };
        let (route, msg, lang, next) = render_validation_outcome(&v, 0);
        assert_eq!(route, "simple_gemma");
        assert!(msg.is_none());
        assert_eq!(lang, "ko");
        assert_eq!(next, 0);
    }

    #[test]
    fn render_outcome_ask_user_to_repeat_bumps_counter_and_returns_korean_message() {
        use crate::voice::pipeline::LanguageCode;
        use crate::voice::voice_chat_pipeline::{QueryRoute, ValidationResult};
        let v = ValidationResult {
            is_valid: false,
            confidence: 0.4,
            is_simple_query: false,
            route: QueryRoute::AskUserToRepeat,
            interpreted_meaning: String::new(),
            detected_language: LanguageCode::Ko,
            validation_time_ms: 1,
        };
        let (route, msg, lang, next) = render_validation_outcome(&v, 0);
        assert_eq!(route, "ask_user_to_repeat");
        assert!(msg.unwrap().contains("잘 들리지 않습니다"));
        assert_eq!(lang, "ko");
        assert_eq!(next, 1);
    }

    #[test]
    fn render_outcome_confirm_interpretation_quotes_paraphrase_in_japanese() {
        use crate::voice::pipeline::LanguageCode;
        use crate::voice::voice_chat_pipeline::{QueryRoute, ValidationResult};
        let v = ValidationResult {
            is_valid: false,
            confidence: 0.5,
            is_simple_query: false,
            route: QueryRoute::ConfirmInterpretation,
            interpreted_meaning: "今日の天気を聞いている".to_string(),
            detected_language: LanguageCode::Ja,
            validation_time_ms: 1,
        };
        let (route, msg, lang, next) = render_validation_outcome(&v, 1);
        assert_eq!(route, "confirm_interpretation");
        let m = msg.expect("non-empty paraphrase yields a message");
        assert!(m.starts_with("もしかして、"), "Japanese prefix expected: {m}");
        assert!(m.contains("'今日の天気を聞いている'"));
        assert_eq!(lang, "ja");
        assert_eq!(next, 2);
    }

    #[test]
    fn render_outcome_complex_llm_resets_counter() {
        use crate::voice::pipeline::LanguageCode;
        use crate::voice::voice_chat_pipeline::{QueryRoute, ValidationResult};
        let v = ValidationResult {
            is_valid: true,
            confidence: 0.9,
            is_simple_query: false,
            route: QueryRoute::ComplexLlm,
            interpreted_meaning: String::new(),
            detected_language: LanguageCode::En,
            validation_time_ms: 1,
        };
        let (route, msg, lang, next) = render_validation_outcome(&v, 5);
        assert_eq!(route, "complex_llm");
        assert!(msg.is_none());
        assert_eq!(lang, "en");
        assert_eq!(next, 0);
    }

    #[test]
    fn render_outcome_confirm_with_empty_paraphrase_uses_localized_fallback() {
        use crate::voice::pipeline::LanguageCode;
        use crate::voice::voice_chat_pipeline::{QueryRoute, ValidationResult};
        let v = ValidationResult {
            is_valid: false,
            confidence: 0.3,
            is_simple_query: false,
            route: QueryRoute::ConfirmInterpretation,
            interpreted_meaning: "   ".to_string(),
            detected_language: LanguageCode::Ko,
            validation_time_ms: 1,
        };
        let (_, msg, _, _) = render_validation_outcome(&v, 1);
        // Korean fallback message — never the bare "혹시 이렇게 …? ''" form.
        assert!(msg.unwrap().contains("여전히 잘 들리지 않습니다"));
    }
}
