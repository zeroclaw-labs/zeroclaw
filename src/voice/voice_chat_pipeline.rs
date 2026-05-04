//! Gemma 4 voice-chat pipeline with self-validation + STT-error-tolerant LLM fallback.
//!
//! # User-supplied 4-stage flow (2026-05-04)
//!
//! ```text
//! [1] Gemma 4 E4B on-device ASR (already lives in `gemma_asr.rs`)
//!       │
//!       ▼  STT text + confidence
//! [2] Gemma 4 self-validate & route (50-100 ms target)
//!       │  Validation prompt asks:
//!       │    - is the text grammatically complete?
//!       │    - any ambiguous words?
//!       │    - simple_factual / simple_command / complex_reasoning / unclear?
//!       │    - can Gemma answer directly?
//!       │  Returns JSON-only.
//!       │
//!       ├─▶ [Route A] Simple → Gemma direct answer (100-200 ms)
//!       │
//!       └─▶ [Route B] Complex / unclear → cloud LLM with STT-tolerant prompt
//!                       (system prompt instructs the LLM that the input came
//!                        from STT and may contain Korean homophone /
//!                        spacing / ambiguous-word errors like the audit's
//!                        canonical "대한사람" example).
//! ```
//!
//! # Why this layer exists
//!
//! Korean STT on Gemma 4 E2B/E4B is ~95 % accurate but mis-transcribes
//! special vocabulary (national-anthem lyrics, classical Korean,
//! 4-character Sino-Korean compounds, etc.). Sending those raw STT
//! results to the cloud LLM works but is slow (extra round-trip),
//! and answering them on-device with Gemma 4 alone misses the
//! ambiguous cases. This layer:
//!   - Gives Gemma 4 a chance to fix what it can in milliseconds.
//!   - Gives the LLM the context it needs (STT confidence, hint that
//!     this is a transcribed query, not a typed one) so it can
//!     reason through the ambiguous cases gracefully.
//!
//! # MoA-integration choices vs the prototype the user supplied
//!
//! The user's prototype called `reqwest::Client::new()` directly and
//! pinned URLs / model IDs. MoA already has a `Provider` trait
//! (`crate::providers::Provider`) that every cloud + local model goes
//! through, with retry, key resolution, billing hooks, etc. So this
//! module:
//!
//!   - Keeps the prototype's PROMPTS exactly (the user wrote them
//!     deliberately; do not paraphrase).
//!   - Keeps the prototype's ROUTING decision (can_answer && !ambiguous
//!     && simple_* → SimpleGemma; otherwise ComplexLlm).
//!   - Replaces the direct HTTP calls with `Provider::chat_with_system`
//!     so calls flow through MoA's existing infrastructure.
//!
//! The `gemma_stt` step from the prototype is OUT OF SCOPE here: the
//! repo already ships `crate::voice::gemma_asr::GemmaAsrSession`,
//! which is the production STT path. This module's pipeline starts
//! from the SttResult that path produces.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::providers::Provider;

// ── Public types ────────────────────────────────────────────────────

/// One STT pass's output, fed into the validation stage.
///
/// Producers (today: `gemma_asr::GemmaAsrSession`) construct one of
/// these per finalized utterance. `confidence` is on the [0.0, 1.0]
/// scale; producers that don't expose a confidence score should pass
/// 1.0 (fully confident) so the pipeline doesn't penalize them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttResult {
    pub text: String,
    pub confidence: f32,
    pub processing_time_ms: u64,
}

/// Validation outcome from the Gemma self-check stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    /// Mirror of the JSON `is_grammatically_complete` field.
    pub is_valid: bool,
    /// Mirror of the JSON `confidence` field [0.0, 1.0].
    pub confidence: f32,
    /// Convenience: derived from `route == SimpleGemma`.
    pub is_simple_query: bool,
    /// Routing decision from the JSON.
    pub route: QueryRoute,
    /// Wall-clock time the validation step itself took.
    pub validation_time_ms: u64,
}

/// Route decision after validation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryRoute {
    /// Gemma 4 will answer directly on-device.
    SimpleGemma,
    /// Send to the cloud LLM with the STT-tolerant prompt.
    ComplexLlm,
}

/// Full pipeline output — one of these per voice turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceChatResponse {
    pub stt_text: String,
    pub stt_confidence: f32,
    pub answer: String,
    pub route: QueryRoute,
    pub total_latency_ms: u64,
    pub breakdown: LatencyBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyBreakdown {
    pub stt_ms: u64,
    pub validation_ms: u64,
    pub inference_ms: u64,
}

// ── Pipeline driver ─────────────────────────────────────────────────

/// Drives the [validate → branch → answer] half of the user's
/// pipeline. The STT half is owned by the existing
/// `gemma_asr::GemmaAsrSession`; callers feed its
/// `SttEvent::Final { text }` into this driver as an `SttResult`.
pub struct VoiceChatPipeline {
    /// Provider for the on-device Gemma 4 (typically the Ollama
    /// adapter). Used for both stages [2] (validate) and [3-A]
    /// (direct answer).
    gemma: Arc<dyn Provider>,
    /// Gemma 4 model id (e.g. `"gemma4:e4b"`). Reused for validate
    /// and direct-answer calls.
    gemma_model: String,
    /// Provider for the cloud LLM fallback (Anthropic / OpenAI /
    /// Gemini, depending on user account configuration). When `None`,
    /// the ComplexLlm route bails — the caller should treat that as
    /// "answer offline-only with the STT text raw".
    llm: Option<Arc<dyn Provider>>,
    /// Cloud LLM model id (e.g. `"claude-opus-4-7"`).
    llm_model: Option<String>,
}

impl VoiceChatPipeline {
    /// Construct a pipeline. The `llm` / `llm_model` pair is optional;
    /// when absent, the `ComplexLlm` route returns the STT text
    /// verbatim with a deterministic apology (so the caller can still
    /// TTS something).
    pub fn new(
        gemma: Arc<dyn Provider>,
        gemma_model: impl Into<String>,
        llm: Option<Arc<dyn Provider>>,
        llm_model: Option<String>,
    ) -> Self {
        Self {
            gemma,
            gemma_model: gemma_model.into(),
            llm,
            llm_model,
        }
    }

    /// Run stages [2] + [3] for a single STT result. Returns the
    /// answer text + the routing decision + a latency breakdown
    /// useful for telemetry / the on-screen "fast path / cloud path"
    /// indicator.
    pub async fn validate_and_answer(
        &self,
        stt: SttResult,
    ) -> Result<VoiceChatResponse> {
        let total_start = Instant::now();

        let validation = self
            .validate_and_route(&stt)
            .await
            .context("voice-chat self-validation stage failed")?;

        let inference_start = Instant::now();
        let answer = match validation.route {
            QueryRoute::SimpleGemma => {
                debug!("voice-chat: route A — Gemma direct answer");
                self.gemma_direct_answer(&stt.text)
                    .await
                    .context("voice-chat direct-answer stage failed")?
            }
            QueryRoute::ComplexLlm => {
                debug!("voice-chat: route B — cloud LLM with STT-tolerant prompt");
                self.llm_robust_answer(&stt.text, &validation)
                    .await
                    .context("voice-chat LLM fallback stage failed")?
            }
        };
        let inference_ms = inference_start.elapsed().as_millis() as u64;
        let total_ms = total_start.elapsed().as_millis() as u64;

        info!(
            route = ?validation.route,
            stt_ms = stt.processing_time_ms,
            validation_ms = validation.validation_time_ms,
            inference_ms,
            total_ms,
            "voice-chat turn complete"
        );

        Ok(VoiceChatResponse {
            stt_text: stt.text,
            stt_confidence: stt.confidence,
            answer,
            route: validation.route,
            total_latency_ms: total_ms,
            breakdown: LatencyBreakdown {
                stt_ms: stt.processing_time_ms,
                validation_ms: validation.validation_time_ms,
                inference_ms,
            },
        })
    }

    /// Stage [2] — Gemma's own self-check. Calls Gemma with the
    /// JSON-only validation prompt and parses the response. The
    /// prompt is the user's verbatim spec.
    async fn validate_and_route(&self, stt: &SttResult) -> Result<ValidationResult> {
        let start = Instant::now();
        let prompt = build_validation_prompt(&stt.text, stt.confidence);

        // We use a low temperature (0.1) and rely on the prompt's
        // `Respond ONLY in JSON` instruction. The model's default
        // temperature would not be wrong, but 0.1 reduces tail
        // latency a bit on most Ollama backends.
        let raw = self
            .gemma
            .chat_with_system(
                Some(VALIDATION_SYSTEM_PROMPT),
                &prompt,
                &self.gemma_model,
                0.1,
            )
            .await
            .context("Gemma validation chat call failed")?;
        let validation_time_ms = start.elapsed().as_millis() as u64;

        let parsed = parse_validation_json(&raw).unwrap_or_else(|err| {
            // If the SLM mis-formats the JSON we treat it as
            // "unclear → escalate to LLM". This is the safe default:
            // routing-on-failure should send to the more capable
            // model, never silently drop to the smaller one.
            warn!(
                error = %err,
                raw = %raw,
                "voice-chat: validation JSON unparseable; defaulting to ComplexLlm route"
            );
            ParsedValidation {
                is_grammatically_complete: false,
                has_ambiguous_words: true,
                query_type: QueryType::Unclear,
                can_answer_directly: false,
                confidence: 0.0,
            }
        });

        let route = if parsed.can_answer_directly
            && !parsed.has_ambiguous_words
            && matches!(parsed.query_type, QueryType::SimpleFactual | QueryType::SimpleCommand)
        {
            QueryRoute::SimpleGemma
        } else {
            QueryRoute::ComplexLlm
        };

        Ok(ValidationResult {
            is_valid: parsed.is_grammatically_complete,
            confidence: parsed.confidence,
            is_simple_query: matches!(route, QueryRoute::SimpleGemma),
            route,
            validation_time_ms,
        })
    }

    /// Stage [3-A] — Gemma answers the user directly.
    ///
    /// Uses a permissive temperature (0.7) because "direct answer"
    /// implies a conversational reply, not a structured one. If the
    /// model produces nothing, we return a fixed apology so the TTS
    /// stage never hits an empty string (which would just be silence
    /// from the user's perspective).
    async fn gemma_direct_answer(&self, stt_text: &str) -> Result<String> {
        let prompt = format!(
            "사용자 질문: {stt_text}\n\n위 질문에 간결하고 정확하게 답변하세요."
        );
        let response = self
            .gemma
            .chat_with_system(
                Some(GEMMA_DIRECT_ANSWER_SYSTEM_PROMPT),
                &prompt,
                &self.gemma_model,
                0.7,
            )
            .await
            .context("Gemma direct-answer chat call failed")?;
        let trimmed = response.trim();
        if trimmed.is_empty() {
            Ok("답변을 생성할 수 없습니다. 다시 말씀해 주세요.".to_string())
        } else {
            Ok(trimmed.to_string())
        }
    }

    /// Stage [3-B] — cloud LLM fallback. Uses the STT-error-tolerant
    /// system prompt the user supplied; the user prompt branches on
    /// the validation confidence (≥ 0.85 vs < 0.85) per the spec.
    async fn llm_robust_answer(
        &self,
        stt_text: &str,
        validation: &ValidationResult,
    ) -> Result<String> {
        let (llm, model) = match (self.llm.as_ref(), self.llm_model.as_ref()) {
            (Some(llm), Some(model)) => (llm, model),
            _ => {
                // No LLM configured — return a deterministic hand-off
                // apology so the TTS stage produces audible output and
                // the user knows to retry / type. Better than a panic
                // or an empty TTS turn.
                warn!(
                    "voice-chat: ComplexLlm route requested but no cloud LLM \
                     configured; returning offline apology"
                );
                return Ok(format!(
                    "죄송합니다. 네트워크가 끊겨 있어 자세한 답변을 드릴 수 없어요. \
                     말씀하신 내용은 \"{stt_text}\" 로 들었어요. 다시 말씀해 주시거나, \
                     온라인 상태에서 다시 시도해 주세요."
                ));
            }
        };

        let user_prompt = build_llm_user_prompt(stt_text, validation.confidence);

        llm.chat_with_system(
            Some(LLM_STT_TOLERANT_SYSTEM_PROMPT),
            &user_prompt,
            model,
            0.7,
        )
        .await
        .context("Cloud LLM fallback call failed")
    }
}

// ── Prompts (verbatim from the user's 2026-05-04 spec) ──────────────

/// System prompt for the Gemma self-validation step. The user's spec
/// gave the validation work as a single user-prompt string; we hoist
/// the "respond only in JSON" instruction into a system prompt so
/// it's harder for the user-content to dislodge it via prompt
/// injection. The schema fields are unchanged.
const VALIDATION_SYSTEM_PROMPT: &str = "\
당신은 음성인식 결과를 0.1초 안에 검증하는 한국어 검증기입니다.\n\
오직 아래 JSON 형식으로만 답변하세요. 자연어 설명을 추가하지 마세요.\n\
필드 정의:\n\
  is_grammatically_complete: 문장이 문법적으로 완결되었는가\n\
  has_ambiguous_words: 의미가 불분명하거나 STT 오류로 의심되는 단어가 있는가\n\
  query_type: simple_factual | simple_command | complex_reasoning | unclear\n\
  can_answer_directly: 너 자신이 (작은 SLM이) 정확히 답할 수 있는가\n\
  confidence: 위 판단들에 대한 너의 자신감 (0.0~1.0)";

/// System prompt for the Gemma direct-answer step. Prevents the model
/// from going off on a tangent when the validation routed a simple
/// factual / command query to it.
const GEMMA_DIRECT_ANSWER_SYSTEM_PROMPT: &str = "\
당신은 사용자의 음성 질문에 즉시 답변하는 한국어 음성 어시스턴트입니다.\n\
답변은 짧고 정확해야 하며, 불필요한 인사말은 생략하세요.\n\
사용자가 다시 묻지 않도록 한 번에 핵심을 전달하세요.";

/// System prompt for the cloud LLM fallback. The user wrote this
/// verbatim; it is reproduced exactly because every line is
/// load-bearing for the STT-error-tolerance behavior the user is
/// trying to elicit.
const LLM_STT_TOLERANT_SYSTEM_PROMPT: &str = "\
당신은 음성 채팅 어시스턴트입니다.\n\
\n\
중요 지침:\n\
1. 사용자 입력은 음성인식(STT)으로 변환된 텍스트입니다\n\
2. 다음과 같은 오류가 있을 수 있습니다:\n\
   - 오탈자 (예: \"대한사람\" → \"대한 사람\" 또는 \"대한민국 사람\")\n\
   - 동음이의어 오류 (예: \"가르치다\" → \"갈리치다\")\n\
   - 띄어쓰기 오류\n\
   - 불완전한 문장\n\
3. 문맥을 통해 사용자의 의도를 파악하여 답변하세요\n\
4. 음성학적으로 유사한 단어 대체 가능성을 고려하세요\n\
5. 의미가 모호할 경우, 가장 가능성 높은 해석으로 답변하되 불확실하면 명확히 물어보세요\n\
6. STT 오류를 지적하지 말고 자연스럽게 올바른 답변을 제공하세요\n\
\n\
답변 시 다음을 포함하세요:\n\
- 사용자가 원하는 정보를 정확하게 제공\n\
- 추가 설명이 필요한 경우 간결하게 덧붙임\n\
- 불확실한 부분은 질문으로 확인";

/// Build the validation user-prompt from the STT text + confidence.
/// User's spec has the schema inside the prompt; we duplicate it in
/// the system prompt above for injection-resistance and keep the
/// user-message minimal.
fn build_validation_prompt(stt_text: &str, confidence: f32) -> String {
    format!(
        "STT 결과: \"{stt_text}\"\n신뢰도: {pct:.2}%",
        pct = confidence * 100.0
    )
}

/// Build the LLM user-prompt with the user-spec's confidence-branch.
/// The two arms are deliberately different: high-confidence is just
/// "answer this", low-confidence prepends a hint that warns the LLM
/// to be more forgiving.
fn build_llm_user_prompt(stt_text: &str, confidence: f32) -> String {
    if confidence < 0.85 {
        format!(
            "[음성인식 신뢰도: {pct:.1}% - 오류 가능성 있음]\n\n\
             사용자 음성 입력:\n\"{stt_text}\"\n\n\
             위 입력에서 사용자가 원하는 것이 무엇인지 파악하여 답변해주세요.",
            pct = confidence * 100.0
        )
    } else {
        format!("사용자 질문:\n\"{stt_text}\"\n\n위 질문에 답변해주세요.")
    }
}

// ── Validation JSON parsing ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryType {
    SimpleFactual,
    SimpleCommand,
    ComplexReasoning,
    Unclear,
}

impl QueryType {
    fn from_str(s: &str) -> Self {
        match s {
            "simple_factual" => Self::SimpleFactual,
            "simple_command" => Self::SimpleCommand,
            "complex_reasoning" => Self::ComplexReasoning,
            _ => Self::Unclear,
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedValidation {
    is_grammatically_complete: bool,
    has_ambiguous_words: bool,
    query_type: QueryType,
    can_answer_directly: bool,
    confidence: f32,
}

/// Robust JSON extraction from a model reply. Real SLMs sometimes
/// wrap JSON in code fences (```json ... ```) or add a leading
/// "Sure, here:" line; we extract the first `{...}` block we see and
/// parse that. Returns Err on no-JSON-found or malformed JSON.
fn parse_validation_json(raw: &str) -> Result<ParsedValidation> {
    let trimmed = raw.trim();
    let start = trimmed
        .find('{')
        .context("validation reply has no opening brace")?;
    let end = trimmed
        .rfind('}')
        .context("validation reply has no closing brace")?;
    if end <= start {
        anyhow::bail!("validation reply braces in wrong order");
    }
    let json_slice = &trimmed[start..=end];

    let v: serde_json::Value = serde_json::from_str(json_slice)
        .with_context(|| format!("could not parse validation JSON: {json_slice}"))?;

    Ok(ParsedValidation {
        is_grammatically_complete: v
            .get("is_grammatically_complete")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        has_ambiguous_words: v
            .get("has_ambiguous_words")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        query_type: v
            .get("query_type")
            .and_then(|x| x.as_str())
            .map(QueryType::from_str)
            .unwrap_or(QueryType::Unclear),
        can_answer_directly: v
            .get("can_answer_directly")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        confidence: v
            .get("confidence")
            .and_then(|x| x.as_f64())
            .map(|f| f as f32)
            .unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_simple_when_can_answer_and_clear_and_simple_factual() {
        let parsed = ParsedValidation {
            is_grammatically_complete: true,
            has_ambiguous_words: false,
            query_type: QueryType::SimpleFactual,
            can_answer_directly: true,
            confidence: 0.92,
        };
        let route = if parsed.can_answer_directly
            && !parsed.has_ambiguous_words
            && matches!(parsed.query_type, QueryType::SimpleFactual | QueryType::SimpleCommand)
        {
            QueryRoute::SimpleGemma
        } else {
            QueryRoute::ComplexLlm
        };
        assert_eq!(route, QueryRoute::SimpleGemma);
    }

    #[test]
    fn route_to_llm_when_ambiguous_word_flagged() {
        // The "대한사람" case from the user's spec: Gemma's check
        // says "yes, can answer" but flags ambiguous_words=true.
        // We MUST escalate.
        let parsed = ParsedValidation {
            is_grammatically_complete: true,
            has_ambiguous_words: true,
            query_type: QueryType::SimpleFactual,
            can_answer_directly: true,
            confidence: 0.7,
        };
        let route = if parsed.can_answer_directly
            && !parsed.has_ambiguous_words
            && matches!(parsed.query_type, QueryType::SimpleFactual | QueryType::SimpleCommand)
        {
            QueryRoute::SimpleGemma
        } else {
            QueryRoute::ComplexLlm
        };
        assert_eq!(route, QueryRoute::ComplexLlm);
    }

    #[test]
    fn route_to_llm_when_complex_reasoning_even_if_clear() {
        let parsed = ParsedValidation {
            is_grammatically_complete: true,
            has_ambiguous_words: false,
            query_type: QueryType::ComplexReasoning,
            can_answer_directly: false,
            confidence: 0.95,
        };
        let route = if parsed.can_answer_directly
            && !parsed.has_ambiguous_words
            && matches!(parsed.query_type, QueryType::SimpleFactual | QueryType::SimpleCommand)
        {
            QueryRoute::SimpleGemma
        } else {
            QueryRoute::ComplexLlm
        };
        assert_eq!(route, QueryRoute::ComplexLlm);
    }

    #[test]
    fn parse_validation_extracts_clean_json() {
        let raw = r#"{
            "is_grammatically_complete": true,
            "has_ambiguous_words": false,
            "query_type": "simple_factual",
            "can_answer_directly": true,
            "confidence": 0.92
        }"#;
        let parsed = parse_validation_json(raw).unwrap();
        assert!(parsed.is_grammatically_complete);
        assert!(!parsed.has_ambiguous_words);
        assert_eq!(parsed.query_type, QueryType::SimpleFactual);
        assert!(parsed.can_answer_directly);
        assert!((parsed.confidence - 0.92).abs() < 1e-4);
    }

    #[test]
    fn parse_validation_strips_code_fence_and_prose() {
        let raw = "물론입니다. 검증 결과는 다음과 같습니다:\n```json\n{\n\
                   \"is_grammatically_complete\": false,\n\
                   \"has_ambiguous_words\": true,\n\
                   \"query_type\": \"unclear\",\n\
                   \"can_answer_directly\": false,\n\
                   \"confidence\": 0.4\n}\n```";
        let parsed = parse_validation_json(raw).unwrap();
        assert!(!parsed.is_grammatically_complete);
        assert!(parsed.has_ambiguous_words);
        assert_eq!(parsed.query_type, QueryType::Unclear);
        assert!(!parsed.can_answer_directly);
    }

    #[test]
    fn parse_validation_treats_unknown_query_type_as_unclear() {
        let raw = r#"{
            "is_grammatically_complete": true,
            "has_ambiguous_words": false,
            "query_type": "weather_query_subtype_3",
            "can_answer_directly": true,
            "confidence": 0.7
        }"#;
        let parsed = parse_validation_json(raw).unwrap();
        assert_eq!(parsed.query_type, QueryType::Unclear);
    }

    #[test]
    fn parse_validation_returns_error_on_garbage() {
        assert!(parse_validation_json("hello, no json here").is_err());
        assert!(parse_validation_json("}{").is_err());
    }

    #[test]
    fn build_llm_user_prompt_high_confidence_omits_warning() {
        let p = build_llm_user_prompt("내일 일정 알려줘", 0.95);
        assert!(p.contains("내일 일정 알려줘"));
        assert!(!p.contains("오류 가능성 있음"));
    }

    #[test]
    fn build_llm_user_prompt_low_confidence_includes_warning() {
        let p = build_llm_user_prompt("대한사람 역사", 0.6);
        assert!(p.contains("대한사람 역사"));
        assert!(p.contains("오류 가능성 있음"));
        // The warning must echo the actual confidence, not a hard-coded
        // sample, so the LLM knows how much to discount the input.
        assert!(p.contains("60.0%"));
    }

    #[test]
    fn build_validation_prompt_contains_text_and_pct() {
        let p = build_validation_prompt("안녕하세요", 0.83);
        assert!(p.contains("안녕하세요"));
        assert!(p.contains("83.00%"));
    }
}
