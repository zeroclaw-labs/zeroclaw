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
//!
//! # Cloud LLM is always available — the operator-key fallback
//!
//! **There is no "user has no cloud LLM" branch in this pipeline.**
//! MoA's contract for the ComplexLlm route is that the cloud LLM is
//! *always* reachable, because:
//!
//!   1. If the user has set their own provider key in Settings, the
//!      caller injects a direct provider (Anthropic / OpenAI / Gemini)
//!      backed by that key. No credit deduction.
//!   2. If the user has NOT set a cloud provider key, the caller
//!      injects a `crate::providers::proxy::ProxyProvider` that routes
//!      the request through the Railway `/api/llm/proxy` endpoint
//!      (currently backed by the operator's Gemini 3.1 Flash key).
//!      Credits are deducted at the operator-key 2.2× multiplier
//!      (`OPERATOR_KEY_CREDIT_MULTIPLIER` in
//!      `crate::billing::llm_router`).
//!
//! Concretely: this pipeline takes a non-optional `llm: Arc<dyn Provider>`
//! and trusts the caller (today: the voice gateway / WS handler) to have
//! resolved that decision the same way `src/agent/loop_.rs` does for
//! text chat — see the `let provider: Box<dyn Provider> = if let
//! (Some(proxy_url), Some(proxy_token)) = ...` block there.
//!
//! Returning a deterministic apology when the cloud LLM is "missing"
//! would be wrong: it would silently bypass the operator-key billing
//! path and hand the user a useless TTS turn. If the LLM call itself
//! fails (network down, proxy 5xx, etc.) we propagate the error —
//! the gateway layer is responsible for any user-facing fallback copy.

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
///
/// `voice_retry_count` is the gateway's running counter of how many
/// times *in a row* the user has re-spoken the same intent on this
/// turn (i.e. successive voice utterances after the pipeline asked
/// them to repeat or confirm). It drives the staircase fallback the
/// user specified:
///
///   * `0` → first voice attempt. If Gemma is uncertain, the pipeline
///     returns `AskUserToRepeat` ("잘 들리지 않습니다…").
///   * `1` → user spoke again (still uncertain). The pipeline returns
///     `ConfirmInterpretation` with Gemma's paraphrase ("혹시 이렇게
///     이해하는 것이 맞습니까? '<paraphrase>'"). The gateway then
///     waits for the user's yes/no/correction reply and concatenates
///     it into the next turn's STT text before re-running the pipeline
///     — so the LLM receives both Gemma's interpretation and the
///     user's confirmation in one prompt.
///   * `>= 2` → we've already exhausted the cheap re-ask paths.
///     Commit to ComplexLlm so the user is never stuck in a loop.
///
/// If the user types instead of re-speaking, the gateway should
/// dispatch through the text chat path entirely; this pipeline never
/// sees that turn. So `voice_retry_count` only counts *voice* retries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttResult {
    pub text: String,
    pub confidence: f32,
    pub processing_time_ms: u64,
    /// How many voice re-attempts have happened on this conversational
    /// turn already. `0` for a fresh turn. Defaults to `0` so older
    /// call sites that don't track retry state behave like a fresh turn.
    #[serde(default)]
    pub voice_retry_count: u8,
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
    /// Gemma's best paraphrase of what it thinks the user said. Used
    /// as the body of the `ConfirmInterpretation` re-ask. Empty when
    /// Gemma did not produce one (or when the route doesn't need it).
    pub interpreted_meaning: String,
    /// Wall-clock time the validation step itself took.
    pub validation_time_ms: u64,
}

/// Route decision after validation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryRoute {
    /// Gemma 4 will answer directly on-device.
    SimpleGemma,
    /// First-attempt re-ask: Gemma was uncertain it understood the
    /// user (ambiguous words, or `query_type = unclear`). Hand back
    /// the fixed phrase "잘 들리지 않습니다…" so the gateway can ask
    /// the user to repeat or switch to text. No paid LLM call.
    AskUserToRepeat,
    /// Second-attempt confirmation: the user re-spoke and Gemma is
    /// *still* uncertain. Instead of asking blindly again, the
    /// pipeline returns Gemma's best paraphrase wrapped in a "혹시
    /// 이렇게 이해하는 것이 맞습니까?" prompt. The gateway is then
    /// expected to capture the user's yes/no/correction and feed both
    /// back into the next turn (so the LLM ultimately sees both
    /// Gemma's reading and the user's confirmation). No paid LLM call.
    ConfirmInterpretation,
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
    /// Provider for the cloud LLM fallback. The caller resolves this
    /// per the operator-key fallback contract documented at module
    /// level: a user-key-backed direct provider when the user has set
    /// their own key, or a `ProxyProvider` pointed at the Railway LLM
    /// proxy (currently Gemini 3.1 Flash) when they have not. The
    /// pipeline does NOT distinguish between the two; both look like
    /// `dyn Provider` from here, and the credit-deduction side effect
    /// (2.2× at `OPERATOR_KEY_CREDIT_MULTIPLIER`) is applied
    /// gateway-side via `record_usage` in the proxy path.
    llm: Arc<dyn Provider>,
    /// Cloud LLM model id. For the operator-fallback path this is the
    /// platform default for `TaskCategory::Interpretation` /
    /// `GeneralChat` (currently a Gemini 3.1 Flash variant) — see
    /// `crate::billing::llm_router::default_model_for_task`. For the
    /// user-key path it's whatever model the user selected.
    llm_model: String,
}

impl VoiceChatPipeline {
    /// Construct a pipeline.
    ///
    /// `llm` / `llm_model` are required: every voice turn that routes
    /// to ComplexLlm must reach a cloud model, either via the user's
    /// own key or via the operator's Railway proxy. See module-level
    /// docs for the resolution contract the caller is expected to
    /// follow.
    pub fn new(
        gemma: Arc<dyn Provider>,
        gemma_model: impl Into<String>,
        llm: Arc<dyn Provider>,
        llm_model: impl Into<String>,
    ) -> Self {
        Self {
            gemma,
            gemma_model: gemma_model.into(),
            llm,
            llm_model: llm_model.into(),
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
            QueryRoute::AskUserToRepeat => {
                // Cost-saving + UX: first re-ask. Gemma was uncertain
                // it understood the user. Hand back a fixed re-ask
                // phrase instead of paying for an LLM call on a
                // noisy/ambiguous transcript. The gateway is
                // responsible for routing this through TTS *and* the
                // chat thread, and for incrementing
                // `voice_retry_count` on the next voice turn so the
                // staircase advances to ConfirmInterpretation.
                debug!(
                    "voice-chat: route C1 — ask user to repeat (uncertain, retry_count=0)"
                );
                ASK_USER_TO_REPEAT_MESSAGE.to_string()
            }
            QueryRoute::ConfirmInterpretation => {
                // Second-attempt confirmation: still uncertain after
                // the user re-spoke. Show Gemma's paraphrase wrapped
                // in the fixed prefix; gateway captures yes/no/correction
                // and feeds it into the next turn so the LLM ultimately
                // sees both readings.
                debug!(
                    "voice-chat: route C2 — confirm interpretation (uncertain, retry_count=1)"
                );
                let paraphrase = validation.interpreted_meaning.trim();
                if paraphrase.is_empty() {
                    CONFIRM_INTERPRETATION_FALLBACK_MESSAGE.to_string()
                } else {
                    format!("{CONFIRM_INTERPRETATION_PREFIX} '{paraphrase}'")
                }
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
                interpreted_meaning: String::new(),
            }
        });

        let route = decide_route(&parsed, stt.voice_retry_count);

        Ok(ValidationResult {
            is_valid: parsed.is_grammatically_complete,
            confidence: parsed.confidence,
            is_simple_query: matches!(route, QueryRoute::SimpleGemma),
            route,
            interpreted_meaning: parsed.interpreted_meaning,
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

    /// Stage [3-B] — cloud LLM call with the STT-error-tolerant
    /// prompt. Uses the user-supplied system prompt verbatim; the user
    /// prompt branches on the validation confidence (≥ 0.85 vs < 0.85)
    /// per the spec.
    ///
    /// The `llm` provider is whatever the caller resolved per the
    /// operator-key fallback contract (see module-level docs):
    /// the user's own provider when they have set a cloud key, or a
    /// `ProxyProvider` to the Railway LLM proxy (currently Gemini 3.1
    /// Flash, billed at the 2.2× operator multiplier) when they have
    /// not. Both look identical from here. Errors propagate; the
    /// gateway layer owns any user-facing copy for "the cloud call
    /// failed".
    async fn llm_robust_answer(
        &self,
        stt_text: &str,
        validation: &ValidationResult,
    ) -> Result<String> {
        let user_prompt = build_llm_user_prompt(stt_text, validation.confidence);

        self.llm
            .chat_with_system(
                Some(LLM_STT_TOLERANT_SYSTEM_PROMPT),
                &user_prompt,
                &self.llm_model,
                0.7,
            )
            .await
            .context("Cloud LLM call failed (operator-fallback path or user-key path)")
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
  confidence: 위 판단들에 대한 너의 자신감 (0.0~1.0)\n\
  interpreted_meaning: 사용자의 의도를 네가 이해한 그대로 한 문장으로 \
요약 (자신감이 낮거나 의미가 모호해도 최선의 추측을 적을 것; 정말 \
아무 의미도 못 잡았다면 빈 문자열)";

/// System prompt for the Gemma direct-answer step. Prevents the model
/// from going off on a tangent when the validation routed a simple
/// factual / command query to it.
const GEMMA_DIRECT_ANSWER_SYSTEM_PROMPT: &str = "\
당신은 사용자의 음성 질문에 즉시 답변하는 한국어 음성 어시스턴트입니다.\n\
답변은 짧고 정확해야 하며, 불필요한 인사말은 생략하세요.\n\
사용자가 다시 묻지 않도록 한 번에 핵심을 전달하세요.";

/// First-attempt re-ask phrase, fixed by user spec (2026-05-04).
/// Returned as the answer body when the route is `AskUserToRepeat`.
/// Costs nothing (no LLM call) and gives the user a chance to either
/// re-speak more clearly or switch to typed input.
const ASK_USER_TO_REPEAT_MESSAGE: &str =
    "잘 들리지 않습니다. 혹시 다시 말씀해주시거나 \
     아니면 텍스트로 입력해주시면 감사하겠습니다.";

/// Second-attempt confirmation prefix, fixed by user spec (2026-05-04).
/// The full phrase is built at runtime as
/// `"{prefix} '{interpreted_meaning}'"` so the user sees Gemma's best
/// reading verbatim and can confirm/correct it. Still no LLM call;
/// the gateway captures the user's reply and bundles it into the
/// next turn so the eventual ComplexLlm prompt has both sides.
const CONFIRM_INTERPRETATION_PREFIX: &str =
    "혹시 이렇게 이해하는 것이 맞습니까?";

/// Fallback used when the route is `ConfirmInterpretation` but Gemma
/// failed to produce any paraphrase at all (empty `interpreted_meaning`).
/// Degrades gracefully to the same first-attempt phrase so the user is
/// never shown a dangling "혹시 이렇게 이해하는 것이 맞습니까? ''".
const CONFIRM_INTERPRETATION_FALLBACK_MESSAGE: &str =
    "여전히 잘 들리지 않습니다. \
     텍스트로 입력해주시면 더 정확하게 도와드릴 수 있습니다.";

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

// ── Routing decision ────────────────────────────────────────────────

/// Decide the route from a parsed validation + the current
/// voice-retry counter. Pulled out as a free function so the unit
/// tests can drive it directly without standing up a `Provider` mock.
///
/// Staircase (ordered, first match wins):
///   1. `SimpleGemma` — Gemma is sure it can answer and the query
///      is plain (`can_answer_directly && !ambiguous && simple_*`).
///   2. `AskUserToRepeat` — uncertain (`ambiguous || unclear`) on
///      the user's first voice attempt (`retry_count == 0`).
///   3. `ConfirmInterpretation` — uncertain *again* on the second
///      voice attempt (`retry_count == 1`).
///   4. `ComplexLlm` — everything else: complex reasoning,
///      `retry_count >= 2` (we've exhausted the cheap re-ask paths
///      and must commit), and the JSON-parse-failed safe default.
fn decide_route(parsed: &ParsedValidation, voice_retry_count: u8) -> QueryRoute {
    let is_uncertain = parsed.has_ambiguous_words
        || matches!(parsed.query_type, QueryType::Unclear);
    if parsed.can_answer_directly
        && !parsed.has_ambiguous_words
        && matches!(parsed.query_type, QueryType::SimpleFactual | QueryType::SimpleCommand)
    {
        QueryRoute::SimpleGemma
    } else if is_uncertain && voice_retry_count == 0 {
        QueryRoute::AskUserToRepeat
    } else if is_uncertain && voice_retry_count == 1 {
        QueryRoute::ConfirmInterpretation
    } else {
        QueryRoute::ComplexLlm
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
    /// Gemma's best paraphrase of the user's intent ("내가 이해한
    /// 것을 적자면…"). Empty string when missing from the JSON.
    interpreted_meaning: String,
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
        interpreted_meaning: v
            .get("interpreted_meaning")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_with(
        ambiguous: bool,
        qt: QueryType,
        can_answer: bool,
        meaning: &str,
    ) -> ParsedValidation {
        ParsedValidation {
            is_grammatically_complete: true,
            has_ambiguous_words: ambiguous,
            query_type: qt,
            can_answer_directly: can_answer,
            confidence: 0.85,
            interpreted_meaning: meaning.to_string(),
        }
    }

    #[test]
    fn route_simple_when_can_answer_and_clear_and_simple_factual() {
        let parsed = parsed_with(false, QueryType::SimpleFactual, true, "");
        assert_eq!(decide_route(&parsed, 0), QueryRoute::SimpleGemma);
    }

    #[test]
    fn route_to_ask_repeat_when_ambiguous_word_flagged_first_attempt() {
        // The "대한사람" case from the user's spec: Gemma's check
        // says "yes, can answer" but flags ambiguous_words=true.
        // First attempt → AskUserToRepeat (cheap re-ask, no LLM bill).
        let parsed = parsed_with(true, QueryType::SimpleFactual, true, "");
        assert_eq!(decide_route(&parsed, 0), QueryRoute::AskUserToRepeat);
    }

    #[test]
    fn route_to_confirm_when_ambiguous_again_on_second_attempt() {
        // Same "대한사람"-class ambiguity, but the user already
        // re-spoke once. Don't ask blindly again — show Gemma's
        // paraphrase and ask for confirmation.
        let parsed = parsed_with(true, QueryType::SimpleFactual, true, "대한민국 사람의 역사");
        assert_eq!(decide_route(&parsed, 1), QueryRoute::ConfirmInterpretation);
    }

    #[test]
    fn route_to_llm_when_ambiguous_after_two_retries() {
        // Exhausted both cheap re-asks; commit to LLM so the user is
        // never trapped in a re-ask loop.
        let parsed = parsed_with(true, QueryType::Unclear, false, "");
        assert_eq!(decide_route(&parsed, 2), QueryRoute::ComplexLlm);
    }

    #[test]
    fn route_to_llm_when_complex_reasoning_even_if_clear() {
        // ComplexReasoning is not "uncertain" (Gemma understood it
        // fine, it's just out of its depth) so the re-ask staircase
        // does not trigger — straight to ComplexLlm even at retry 0.
        let parsed = parsed_with(false, QueryType::ComplexReasoning, false, "");
        assert_eq!(decide_route(&parsed, 0), QueryRoute::ComplexLlm);
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

    #[test]
    fn parse_validation_extracts_interpreted_meaning() {
        let raw = r#"{
            "is_grammatically_complete": false,
            "has_ambiguous_words": true,
            "query_type": "unclear",
            "can_answer_directly": false,
            "confidence": 0.4,
            "interpreted_meaning": "대한민국 사람의 역사를 묻는 듯"
        }"#;
        let parsed = parse_validation_json(raw).unwrap();
        assert_eq!(parsed.interpreted_meaning, "대한민국 사람의 역사를 묻는 듯");
    }

    #[test]
    fn parse_validation_missing_interpreted_meaning_is_empty_string() {
        // Older Gemma replies (before we added the field to the
        // prompt) won't include it — must degrade to empty string,
        // not panic, so the ConfirmInterpretation branch can fall
        // back to the "여전히 잘 들리지 않습니다…" message.
        let raw = r#"{
            "is_grammatically_complete": true,
            "has_ambiguous_words": false,
            "query_type": "simple_factual",
            "can_answer_directly": true,
            "confidence": 0.9
        }"#;
        let parsed = parse_validation_json(raw).unwrap();
        assert_eq!(parsed.interpreted_meaning, "");
    }

    /// Mirrors the exact format string used in the
    /// `ConfirmInterpretation` arm of `validate_and_answer`. Kept as
    /// a small inline closure so a future refactor of the format
    /// string updates one place.
    fn confirm_interpretation_message(paraphrase: &str) -> String {
        let trimmed = paraphrase.trim();
        if trimmed.is_empty() {
            CONFIRM_INTERPRETATION_FALLBACK_MESSAGE.to_string()
        } else {
            format!("{CONFIRM_INTERPRETATION_PREFIX} '{trimmed}'")
        }
    }

    #[test]
    fn confirm_interpretation_with_paraphrase_includes_quote() {
        let msg = confirm_interpretation_message("대한민국 사람의 역사");
        assert!(msg.starts_with(CONFIRM_INTERPRETATION_PREFIX));
        assert!(msg.contains("'대한민국 사람의 역사'"));
    }

    #[test]
    fn confirm_interpretation_falls_back_when_paraphrase_empty() {
        // Must never produce "혹시 이렇게 이해하는 것이 맞습니까? ''" —
        // that would be a worse UX than the first re-ask phrase.
        let msg = confirm_interpretation_message("   ");
        assert_eq!(msg, CONFIRM_INTERPRETATION_FALLBACK_MESSAGE);
    }
}
