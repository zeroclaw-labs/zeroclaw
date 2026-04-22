//! SLM gatekeeper routing engine.
//!
//! Classifies user messages locally and routes to the appropriate handler:
//! simple tasks stay local, complex tasks are delegated to cloud LLMs.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Default Ollama endpoint for the local SLM.
const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434/v1";

/// Default SLM model name.
const DEFAULT_SLM_MODEL: &str = "qwen3:0.6b";

/// Confidence threshold below which we delegate to cloud.
/// Fallback threshold used by `GatekeeperRouter::new` and tests when
/// no `GatekeeperConfig` is plumbed in. Production code reads
/// `GatekeeperConfig::confidence_threshold` instead via `from_config`.
const DEFAULT_CONFIDENCE_THRESHOLD: f64 = 0.6;

// ── SLM task types ───────────────────────────────────────────────

/// Tasks the local SLM can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SlmTask {
    /// Classify user intent into a structured category.
    IntentClassification,
    /// Detect greetings and respond directly.
    GreetingDetection,
    /// Check for pending tasks ("any work waiting?").
    HeartbeatCheck,
    /// Detect sensitive/private data patterns.
    PrivacyDetection,
    /// Determine which tool should be invoked.
    ToolRouting,
    /// Summarize context and delegate to cloud LLM.
    CloudDelegation,
}

// ── Task category ────────────────────────────────────────────────

/// Complexity category for routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskCategory {
    /// SLM handles directly (greetings, simple Q&A).
    Simple,
    /// Tool call + SLM assembles response.
    Medium,
    /// Cloud LLM required (reasoning, coding, analysis).
    Complex,
    /// Specialized tool required (legal RAG, voice, etc.).
    Specialized,
}

/// Routing target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingTarget {
    /// Handle locally with SLM.
    Local,
    /// Delegate to cloud LLM.
    Cloud,
}

// ── Routing decision ─────────────────────────────────────────────

/// Result of the gatekeeper's routing analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// Task complexity category.
    pub category: TaskCategory,
    /// Tool needed, if any.
    pub tool_needed: Option<String>,
    /// Routing target (local or cloud).
    pub target: RoutingTarget,
    /// Raw classifier confidence (0.0 – 1.0) from the keyword
    /// heuristic. See also `weighted_confidence`.
    pub confidence: f64,
    /// Human-readable reason for the decision.
    pub reason: String,
    /// Macro-difficulty inferred by
    /// `routing_policy::classify_question`. `None` when the policy
    /// has not been applied yet (older callers of `classify` that
    /// skip `process_message`). Spec (2026-04-23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<super::routing_policy::QuestionDifficulty>,
    /// Professional domain(s) detected by the classifier (e.g.
    /// Medical + Legal for a medical-malpractice question).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<super::routing_policy::ProfessionalDomain>,
    /// Weighted confidence that the SLM should be trusted for this
    /// specific answer. Set once `process_message` has generated a
    /// draft and evaluated it through
    /// `routing_policy::weighted_confidence`. When `None`, callers
    /// should fall back to comparing `confidence` against the
    /// threshold for backwards compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weighted_confidence: Option<f64>,
}

impl RoutingDecision {
    /// Whether this message should be handled locally.
    pub fn is_local(&self) -> bool {
        self.target == RoutingTarget::Local
    }
}

// ── Cloud delegation ─────────────────────────────────────────────

/// Data package generated when SLM delegates to cloud.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudDelegation {
    /// SLM-summarized conversation context.
    pub context_summary: String,
    /// Description of the task for the cloud model.
    pub task_description: String,
    /// Suggested rephrased user question.
    pub suggested_user_question: String,
}

// ── Offline queue ────────────────────────────────────────────────

/// Entry in the offline task queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTask {
    /// Unique task ID.
    pub id: String,
    /// Original user message.
    pub message: String,
    /// User ID.
    pub user_id: String,
    /// Channel the message came from.
    pub channel: String,
    /// Routing decision that triggered queueing.
    pub routing: RoutingDecision,
    /// Timestamp when queued (epoch seconds).
    pub queued_at: i64,
}

/// Offline task queue for cloud-bound tasks when disconnected.
pub struct OfflineQueue {
    tasks: Arc<Mutex<VecDeque<QueuedTask>>>,
    max_size: usize,
}

impl OfflineQueue {
    /// Create a new offline queue with a maximum capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(VecDeque::new())),
            max_size,
        }
    }

    /// Enqueue a task for later cloud processing.
    pub async fn enqueue(&self, task: QueuedTask) -> anyhow::Result<()> {
        let mut queue = self.tasks.lock().await;
        if queue.len() >= self.max_size {
            anyhow::bail!(
                "Offline queue full ({}/{}). Oldest tasks must be drained first.",
                queue.len(),
                self.max_size
            );
        }
        queue.push_back(task);
        Ok(())
    }

    /// Drain all queued tasks (for batch dispatch after reconnection).
    pub async fn drain_all(&self) -> Vec<QueuedTask> {
        let mut queue = self.tasks.lock().await;
        queue.drain(..).collect()
    }

    /// Peek at the next task without removing it.
    pub async fn peek(&self) -> Option<QueuedTask> {
        let queue = self.tasks.lock().await;
        queue.front().cloned()
    }

    /// Get the number of queued tasks.
    pub async fn len(&self) -> usize {
        let queue = self.tasks.lock().await;
        queue.len()
    }

    /// Check if the queue is empty.
    pub async fn is_empty(&self) -> bool {
        let queue = self.tasks.lock().await;
        queue.is_empty()
    }
}

// ── Keyword-based patterns for local classification ──────────────

/// Greeting patterns (Korean + English).
const GREETING_PATTERNS: &[&str] = &[
    "안녕",
    "하이",
    "헬로",
    "ㅎㅇ",
    "반가",
    "좋은 아침",
    "좋은 저녁",
    "hello",
    "hi",
    "hey",
    "good morning",
    "good evening",
    "howdy",
];

/// Simple query patterns that SLM can handle locally.
const SIMPLE_PATTERNS: &[&str] = &[
    "몇 시",
    "날짜",
    "오늘",
    "내일",
    "뭐야",
    "누구",
    "what time",
    "what day",
    "today",
    "tomorrow",
];

/// Tool-invoking patterns.
const TOOL_PATTERNS: &[(&str, &str)] = &[
    ("날씨", "weather"),
    ("weather", "weather"),
    ("일정", "calendar"),
    ("calendar", "calendar"),
    ("schedule", "calendar"),
    ("검색", "search"),
    ("search", "search"),
    ("찾아", "search"),
    ("알람", "alarm"),
    ("alarm", "alarm"),
    ("타이머", "timer"),
    ("timer", "timer"),
    ("번역", "translate"),
    ("translate", "translate"),
    ("계산", "calculator"),
    ("calculate", "calculator"),
];

/// Complex patterns that require cloud LLM.
const COMPLEX_PATTERNS: &[&str] = &[
    "분석",
    "코드",
    "코딩",
    "프로그래밍",
    "작성",
    "요약",
    "설명해",
    "비교",
    "추천",
    "전략",
    "analyze",
    "code",
    "coding",
    "programming",
    "write",
    "summarize",
    "explain",
    "compare",
    "recommend",
    "strategy",
];

/// Specialized patterns requiring specific tools.
const SPECIALIZED_PATTERNS: &[(&str, &str)] = &[
    ("법률", "legal_rag"),
    ("legal", "legal_rag"),
    ("소송", "legal_rag"),
    ("lawsuit", "legal_rag"),
    ("통역", "voice_interpreter"),
    ("interpret", "voice_interpreter"),
    ("음성", "voice_interpreter"),
    ("voice", "voice_interpreter"),
    ("이미지", "image_gen"),
    ("그림", "image_gen"),
    ("image", "image_gen"),
    ("draw", "image_gen"),
];

/// Privacy-sensitive patterns.
const PRIVACY_PATTERNS: &[&str] = &[
    "주민등록",
    "비밀번호",
    "계좌",
    "카드번호",
    "여권",
    "ssn",
    "password",
    "account number",
    "card number",
    "passport",
    "개인정보",
    "personal info",
];

// ── Gatekeeper router ────────────────────────────────────────────

/// Result of a gatekeeper routing + optional local response.
#[derive(Debug, Clone)]
pub struct GatekeeperResult {
    /// The routing decision.
    pub decision: RoutingDecision,
    /// If the gatekeeper handled the message locally, the SLM response.
    /// `None` means the message should be forwarded to the cloud LLM.
    pub local_response: Option<String>,
}

/// Ollama chat request (native API format, not OpenAI-compatible).
#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f64,
}

/// Ollama chat response.
#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    #[serde(default)]
    content: String,
}

/// The local SLM gatekeeper router.
///
/// Classifies user messages using fast keyword-based heuristics first,
/// then optionally consults the local SLM via Ollama for ambiguous cases.
/// For simple/greeting messages, generates a response locally without cloud calls.
pub struct GatekeeperRouter {
    /// Ollama API endpoint.
    ollama_url: String,
    /// SLM model name.
    model: String,
    /// Whether the SLM backend is available.
    slm_available: bool,
    /// HTTP client for Ollama requests.
    client: reqwest::Client,
    /// Offline task queue.
    queue: OfflineQueue,
    /// Confidence floor for the locally-classified decision. When the
    /// classifier's `confidence` lies BELOW this value, `process_message`
    /// suppresses the local SLM call and returns
    /// `local_response=None` so the chat handler escalates to the
    /// cloud LLM. Pulled from `GatekeeperConfig::confidence_threshold`
    /// at construction time. Spec (2026-04-22).
    confidence_threshold: f64,
}

impl GatekeeperRouter {
    /// Create a new gatekeeper router.
    pub fn new(ollama_url: Option<&str>, model: Option<&str>) -> Self {
        let timeout_secs = 10;
        Self {
            ollama_url: ollama_url.unwrap_or(DEFAULT_OLLAMA_URL).to_string(),
            model: model.unwrap_or(DEFAULT_SLM_MODEL).to_string(),
            slm_available: false,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            queue: OfflineQueue::new(100),
            confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
        }
    }

    /// Create from a `GatekeeperConfig`.
    pub fn from_config(config: &crate::config::GatekeeperConfig) -> Self {
        Self {
            ollama_url: config.ollama_url.clone(),
            model: config.model.clone(),
            slm_available: false,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(config.timeout_secs))
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            queue: OfflineQueue::new(100),
            confidence_threshold: config.confidence_threshold,
        }
    }

    /// Get a reference to the offline queue.
    pub fn queue(&self) -> &OfflineQueue {
        &self.queue
    }

    /// Get the configured model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Get the configured Ollama URL.
    pub fn ollama_url(&self) -> &str {
        &self.ollama_url
    }

    /// Whether the SLM backend was last known to be available.
    pub fn is_slm_available(&self) -> bool {
        self.slm_available
    }

    /// Check if the local SLM is reachable via Ollama.
    pub async fn check_slm_health(&mut self) -> bool {
        // Ollama health check: GET /api/tags
        let url = self.ollama_url.replace("/v1", "/api/tags");
        match self.client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                self.slm_available = true;
                true
            }
            _ => {
                self.slm_available = false;
                false
            }
        }
    }

    /// Generate a response using the local SLM via Ollama.
    ///
    /// Returns `Ok(response)` if the SLM generated a response, or `Err` on failure.
    /// On failure, callers should fall back to the cloud LLM.
    async fn respond_locally(&self, message: &str) -> anyhow::Result<String> {
        let url = self.ollama_url.replace("/v1", "/api/chat");
        let body = OllamaChatRequest {
            model: self.model.clone(),
            messages: vec![
                OllamaMessage {
                    role: "system".to_string(),
                    content: "You are a helpful AI assistant. Respond concisely in the same language as the user. Keep responses under 3 sentences for simple queries.".to_string(),
                },
                OllamaMessage {
                    role: "user".to_string(),
                    content: message.to_string(),
                },
            ],
            stream: false,
            options: OllamaOptions { temperature: 0.7 },
        };

        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("Ollama returned status {}", resp.status());
        }

        let chat_resp: OllamaChatResponse = resp.json().await?;
        let content = chat_resp.message.content.trim().to_string();
        if content.is_empty() {
            anyhow::bail!("Ollama returned empty response");
        }
        Ok(content)
    }

    /// Process a user message end-to-end through the gatekeeper.
    ///
    /// 1. Classifies the message using keyword heuristics.
    /// 2. If routed to `Local` and SLM is available, generates a response locally.
    /// 3. Returns `GatekeeperResult` with routing decision and optional local response.
    ///
    /// If `local_response` is `None`, the caller should forward to the cloud LLM.
    pub async fn process_message(&self, message: &str) -> GatekeeperResult {
        use super::routing_policy;
        // Spec (2026-04-23): the routing decision is now a two-stage
        // weighted judgement instead of a single keyword confidence:
        //
        //   1. `classify_question` categorises the ask as Simple
        //      (web-searchable fact / common knowledge),
        //      Specialized (medical / legal / scientific / math /
        //      coding / finance), or ComplexReasoning (multi-domain
        //      composite / chain-of-thought heavy).
        //   2. The raw keyword classifier (`self.classify`) still runs
        //      to produce a baseline confidence number.
        //   3. If the SLM is even eligible to answer, we generate the
        //      draft answer BEFORE deciding — then evaluate answer
        //      quality (length, hedging markers, tool outcomes) and
        //      compute a weighted_confidence that folds difficulty
        //      bias + quality penalties in. That is what the
        //      threshold gate compares against, not the raw number.
        //
        // The net effect: a Simple fact question where the SLM
        // invoked a web tool and got a clean answer sails through
        // (base + 0.20 bias). A Specialized medical question with a
        // hedged answer eats a large penalty and escalates even if
        // the keyword classifier thought it was confident.
        let classification = routing_policy::classify_question(message);
        let mut decision = self.classify(message);
        decision.difficulty = Some(classification.difficulty);
        decision.domains = classification.domains.clone();

        // Hard exits that short-circuit the whole policy:
        //   * the keyword classifier already routed to Cloud;
        //   * the SLM daemon is unavailable.
        if decision.target != RoutingTarget::Local || !self.slm_available {
            tracing::debug!(
                target = ?decision.target,
                slm_available = self.slm_available,
                difficulty = ?classification.difficulty,
                "Gatekeeper skipping SLM attempt"
            );
            return GatekeeperResult {
                decision,
                local_response: None,
            };
        }

        // Generate the draft answer first — quality evaluation needs
        // the actual text. On SLM failure we escalate immediately
        // (cloud LLM is the safety net).
        let slm_reply = match self.respond_locally(message).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "SLM local response failed, escalating");
                return GatekeeperResult {
                    decision,
                    local_response: None,
                };
            }
        };

        // Evaluate answer quality. `tool_was_used` is threaded as
        // `false` here because the router's `respond_locally` path
        // does not invoke tools directly (the agent loop does, in
        // the chat handler). A future refactor can carry the real
        // tool outcome through so tool failures drive the penalty.
        let quality =
            routing_policy::evaluate_answer_quality(message, &slm_reply, false, None);
        let weighted =
            routing_policy::weighted_confidence(decision.confidence, &classification, &quality);
        decision.weighted_confidence = Some(weighted);

        if weighted < self.confidence_threshold {
            tracing::info!(
                base = decision.confidence,
                weighted,
                threshold = self.confidence_threshold,
                difficulty = ?classification.difficulty,
                hedged = quality.has_hedging,
                tool_success = ?quality.tool_success,
                "Weighted confidence below threshold — escalating to cloud LLM"
            );
            return GatekeeperResult {
                decision,
                local_response: None,
            };
        }

        tracing::info!(
            base = decision.confidence,
            weighted,
            difficulty = ?classification.difficulty,
            "Gatekeeper handled locally via SLM (weighted policy)"
        );
        GatekeeperResult {
            decision,
            local_response: Some(slm_reply),
        }
    }

    /// Classify a user message and decide routing.
    ///
    /// Uses fast keyword heuristics. Falls back to sensible defaults
    /// when SLM is unavailable.
    pub fn classify(&self, message: &str) -> RoutingDecision {
        let lower = message.to_lowercase();

        // 1. Privacy check (always flag, still route normally)
        let has_privacy_concern = PRIVACY_PATTERNS.iter().any(|p| lower.contains(p));

        // 2. Greeting detection → local
        if GREETING_PATTERNS.iter().any(|p| lower.contains(p)) && message.len() < 50 {
            return RoutingDecision {
                category: TaskCategory::Simple,
                tool_needed: None,
                target: RoutingTarget::Local,
                confidence: 0.95,
                reason: "Greeting detected — handled locally".into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            };
        }

        // 3. Specialized tool patterns → cloud + tool
        for (pattern, tool) in SPECIALIZED_PATTERNS {
            if lower.contains(pattern) {
                return RoutingDecision {
                    category: TaskCategory::Specialized,
                    tool_needed: Some((*tool).to_string()),
                    target: RoutingTarget::Cloud,
                    confidence: 0.85,
                    reason: format!("Specialized tool required: {tool}"),
                    difficulty: None,
                    domains: Vec::new(),
                    weighted_confidence: None,
                };
            }
        }

        // 4. Complex patterns → cloud
        if COMPLEX_PATTERNS.iter().any(|p| lower.contains(p)) {
            return RoutingDecision {
                category: TaskCategory::Complex,
                tool_needed: None,
                target: RoutingTarget::Cloud,
                confidence: 0.8,
                reason: "Complex task — requires cloud LLM reasoning".into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            };
        }

        // 5. Tool patterns → medium (local tool dispatch)
        for (pattern, tool) in TOOL_PATTERNS {
            if lower.contains(pattern) {
                return RoutingDecision {
                    category: TaskCategory::Medium,
                    tool_needed: Some((*tool).to_string()),
                    target: RoutingTarget::Local,
                    confidence: 0.8,
                    reason: format!("Tool invocation: {tool}"),
                    difficulty: None,
                    domains: Vec::new(),
                    weighted_confidence: None,
                };
            }
        }

        // 6. Simple patterns → local
        if SIMPLE_PATTERNS.iter().any(|p| lower.contains(p)) {
            return RoutingDecision {
                category: TaskCategory::Simple,
                tool_needed: None,
                target: RoutingTarget::Local,
                confidence: 0.75,
                reason: "Simple query — handled locally".into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            };
        }

        // 7. Privacy-flagged but otherwise unclassified → cloud with caution
        if has_privacy_concern {
            return RoutingDecision {
                category: TaskCategory::Complex,
                tool_needed: None,
                target: RoutingTarget::Cloud,
                confidence: 0.5,
                reason: "Privacy-sensitive content detected — requires careful cloud handling"
                    .into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            };
        }

        // 8. Short messages (< 20 chars) → try local
        if message.chars().count() < 20 {
            return RoutingDecision {
                category: TaskCategory::Simple,
                tool_needed: None,
                target: RoutingTarget::Local,
                confidence: 0.55,
                reason: "Short message — attempting local handling".into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            };
        }

        // 9. Default: delegate to cloud
        RoutingDecision {
            category: TaskCategory::Complex,
            tool_needed: None,
            target: RoutingTarget::Cloud,
            confidence: DEFAULT_CONFIDENCE_THRESHOLD,
            reason: "Ambiguous intent — delegating to cloud LLM".into(),
            difficulty: None,
            domains: Vec::new(),
            weighted_confidence: None,
        }
    }

    /// Check for pending tasks (heartbeat check).
    pub fn heartbeat_check(&self, pending_task_count: usize) -> RoutingDecision {
        if pending_task_count > 0 {
            RoutingDecision {
                category: TaskCategory::Medium,
                tool_needed: None,
                target: RoutingTarget::Local,
                confidence: 1.0,
                reason: format!("{pending_task_count} pending task(s) found"),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            }
        } else {
            RoutingDecision {
                category: TaskCategory::Simple,
                tool_needed: None,
                target: RoutingTarget::Local,
                confidence: 1.0,
                reason: "No pending tasks".into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
            }
        }
    }

    /// Detect privacy-sensitive content in a message.
    pub fn detect_privacy_risk(&self, message: &str) -> Vec<String> {
        let lower = message.to_lowercase();
        PRIVACY_PATTERNS
            .iter()
            .filter(|p| lower.contains(**p))
            .map(|p| (*p).to_string())
            .collect()
    }

    /// Check whether a routing decision requires the voice interpreter.
    ///
    /// Returns `true` when `tool_needed` is `"voice_interpreter"`, indicating
    /// the caller should redirect to the `/ws/voice` WebSocket endpoint instead
    /// of processing the message through the normal chat pipeline.
    ///
    /// # Example
    /// ```ignore
    /// let decision = router.classify("음성 통역 시작");
    /// if router.is_voice_interpreter(&decision) {
    ///     // Redirect client to ws://host/ws/voice
    /// }
    /// ```
    pub fn is_voice_interpreter(decision: &RoutingDecision) -> bool {
        decision
            .tool_needed
            .as_deref()
            .is_some_and(|t| t == "voice_interpreter")
    }

    /// Build a voice interpreter redirect response for chat contexts.
    ///
    /// When the gatekeeper classifies a message as requiring voice interpretation,
    /// callers can use this to generate a user-facing redirect instruction.
    pub fn voice_interpreter_redirect(gateway_host: &str, gateway_port: u16) -> String {
        format!(
            "Voice interpretation is available via WebSocket at ws://{gateway_host}:{gateway_port}/ws/voice — \
             connect with a session_start message to begin simultaneous interpretation."
        )
    }

    /// Generate a cloud delegation payload when SLM determines cloud is needed.
    pub fn prepare_delegation(&self, message: &str, context: Option<&str>) -> CloudDelegation {
        let context_summary = context
            .map(|c| {
                if c.len() > 500 {
                    format!(
                        "{}...",
                        &c[..c.char_indices().nth(500).map_or(c.len(), |(i, _)| i)]
                    )
                } else {
                    c.to_string()
                }
            })
            .unwrap_or_default();

        CloudDelegation {
            context_summary,
            task_description: format!("Process user request: {message}"),
            suggested_user_question: message.to_string(),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_router() -> GatekeeperRouter {
        GatekeeperRouter::new(None, None)
    }

    #[test]
    fn classify_greeting_korean() {
        let router = make_router();
        let decision = router.classify("안녕하세요");
        assert_eq!(decision.category, TaskCategory::Simple);
        assert_eq!(decision.target, RoutingTarget::Local);
        assert!(decision.confidence > 0.9);
        assert!(decision.is_local());
    }

    #[test]
    fn classify_greeting_english() {
        let router = make_router();
        let decision = router.classify("hello there");
        assert_eq!(decision.category, TaskCategory::Simple);
        assert!(decision.is_local());
    }

    #[test]
    fn classify_complex_task() {
        let router = make_router();
        let decision = router.classify("이 코드를 분석해줘");
        assert_eq!(decision.category, TaskCategory::Complex);
        assert_eq!(decision.target, RoutingTarget::Cloud);
        assert!(!decision.is_local());
    }

    #[test]
    fn classify_tool_weather() {
        let router = make_router();
        let decision = router.classify("오늘 날씨 알려줘");
        assert_eq!(decision.category, TaskCategory::Medium);
        assert_eq!(decision.tool_needed, Some("weather".to_string()));
        assert!(decision.is_local());
    }

    #[test]
    fn classify_tool_search() {
        let router = make_router();
        let decision = router.classify("이거 검색해줘");
        assert_eq!(decision.category, TaskCategory::Medium);
        assert_eq!(decision.tool_needed, Some("search".to_string()));
    }

    #[test]
    fn classify_specialized_legal() {
        let router = make_router();
        let decision = router.classify("법률 상담이 필요해요");
        assert_eq!(decision.category, TaskCategory::Specialized);
        assert_eq!(decision.tool_needed, Some("legal_rag".to_string()));
        assert_eq!(decision.target, RoutingTarget::Cloud);
    }

    #[test]
    fn classify_specialized_voice() {
        let router = make_router();
        let decision = router.classify("음성 통역 시작");
        assert_eq!(decision.category, TaskCategory::Specialized);
        assert_eq!(decision.tool_needed, Some("voice_interpreter".to_string()));
    }

    #[test]
    fn is_voice_interpreter_returns_true_for_voice_decision() {
        let router = make_router();
        let decision = router.classify("음성 통역 시작");
        assert!(GatekeeperRouter::is_voice_interpreter(&decision));
    }

    #[test]
    fn is_voice_interpreter_returns_false_for_non_voice_decision() {
        let router = make_router();
        let decision = router.classify("오늘 날씨 알려줘");
        assert!(!GatekeeperRouter::is_voice_interpreter(&decision));
    }

    #[test]
    fn voice_interpreter_redirect_contains_ws_path() {
        let msg = GatekeeperRouter::voice_interpreter_redirect("127.0.0.1", 3000);
        assert!(msg.contains("/ws/voice"));
        assert!(msg.contains("127.0.0.1:3000"));
    }

    #[test]
    fn classify_simple_question() {
        let router = make_router();
        let decision = router.classify("오늘 몇 시야?");
        assert_eq!(decision.category, TaskCategory::Simple);
        assert!(decision.is_local());
    }

    #[test]
    fn classify_short_message() {
        let router = make_router();
        let decision = router.classify("ok");
        assert_eq!(decision.category, TaskCategory::Simple);
        assert!(decision.is_local());
    }

    #[test]
    fn classify_ambiguous_defaults_to_cloud() {
        let router = make_router();
        let decision = router.classify("I have a question about a complex philosophical topic that requires deep reasoning and analysis");
        assert_eq!(decision.target, RoutingTarget::Cloud);
    }

    #[test]
    fn classify_privacy_sensitive() {
        let router = make_router();
        let decision = router.classify("내 주민등록번호가 뭐였지");
        assert_eq!(decision.target, RoutingTarget::Cloud);
        assert!(decision.reason.contains("Privacy"));
    }

    #[test]
    fn detect_privacy_risk_patterns() {
        let router = make_router();
        let risks = router.detect_privacy_risk("내 비밀번호는 1234이고 계좌번호도 알려줘");
        assert!(risks.contains(&"비밀번호".to_string()));
        assert!(risks.contains(&"계좌".to_string()));
    }

    #[test]
    fn detect_privacy_risk_empty() {
        let router = make_router();
        let risks = router.detect_privacy_risk("좋은 날씨네요");
        assert!(risks.is_empty());
    }

    #[test]
    fn heartbeat_with_pending_tasks() {
        let router = make_router();
        let decision = router.heartbeat_check(3);
        assert_eq!(decision.category, TaskCategory::Medium);
        assert!(decision.reason.contains('3'));
    }

    #[test]
    fn heartbeat_no_pending_tasks() {
        let router = make_router();
        let decision = router.heartbeat_check(0);
        assert_eq!(decision.category, TaskCategory::Simple);
        assert!(decision.reason.contains("No pending"));
    }

    #[test]
    fn prepare_delegation_basic() {
        let router = make_router();
        let delegation = router.prepare_delegation("이 코드를 리뷰해줘", None);
        assert!(delegation.task_description.contains("이 코드를 리뷰해줘"));
        assert!(delegation.context_summary.is_empty());
    }

    #[test]
    fn prepare_delegation_with_context() {
        let router = make_router();
        let delegation = router.prepare_delegation("계속해줘", Some("이전 대화 내용"));
        assert!(!delegation.context_summary.is_empty());
        assert!(delegation.context_summary.contains("이전 대화"));
    }

    #[test]
    fn prepare_delegation_truncates_long_context() {
        let router = make_router();
        let long_context = "가".repeat(1000);
        let delegation = router.prepare_delegation("요약해줘", Some(&long_context));
        assert!(delegation.context_summary.len() < long_context.len());
        assert!(delegation.context_summary.ends_with("..."));
    }

    #[test]
    fn router_default_config() {
        let router = make_router();
        assert_eq!(router.model(), "qwen3:0.6b");
        assert_eq!(router.ollama_url(), "http://127.0.0.1:11434/v1");
    }

    #[test]
    fn router_custom_config() {
        let router = GatekeeperRouter::new(Some("http://10.0.0.1:11434/v1"), Some("llama3:latest"));
        assert_eq!(router.model(), "llama3:latest");
        assert_eq!(router.ollama_url(), "http://10.0.0.1:11434/v1");
    }

    #[tokio::test]
    async fn offline_queue_enqueue_and_drain() {
        let queue = OfflineQueue::new(10);

        let task = QueuedTask {
            id: "task-1".into(),
            message: "test message".into(),
            user_id: "zeroclaw_user".into(),
            channel: "kakao".into(),
            routing: RoutingDecision {
                category: TaskCategory::Complex,
                tool_needed: None,
                target: RoutingTarget::Cloud,
                confidence: 0.8,
                reason: "test".into(),
                difficulty: None,
                domains: Vec::new(),
                weighted_confidence: None,
},
            queued_at: 1000,
        };

        queue.enqueue(task).await.unwrap();
        assert_eq!(queue.len().await, 1);
        assert!(!queue.is_empty().await);

        let peeked = queue.peek().await;
        assert!(peeked.is_some());
        assert_eq!(peeked.unwrap().id, "task-1");

        let drained = queue.drain_all().await;
        assert_eq!(drained.len(), 1);
        assert!(queue.is_empty().await);
    }

    #[tokio::test]
    async fn offline_queue_enforces_max_size() {
        let queue = OfflineQueue::new(2);

        for i in 0..2 {
            queue
                .enqueue(QueuedTask {
                    id: format!("task-{i}"),
                    message: "msg".into(),
                    user_id: "zeroclaw_user".into(),
                    channel: "test".into(),
                    routing: RoutingDecision {
                        category: TaskCategory::Simple,
                        tool_needed: None,
                        target: RoutingTarget::Cloud,
                        confidence: 0.5,
                        reason: "test".into(),
                        difficulty: None,
                        domains: Vec::new(),
                        weighted_confidence: None,
},
                    queued_at: 0,
                })
                .await
                .unwrap();
        }

        let result = queue
            .enqueue(QueuedTask {
                id: "task-overflow".into(),
                message: "overflow".into(),
                user_id: "zeroclaw_user".into(),
                channel: "test".into(),
                routing: RoutingDecision {
                    category: TaskCategory::Simple,
                    tool_needed: None,
                    target: RoutingTarget::Cloud,
                    confidence: 0.5,
                    reason: "test".into(),
                    difficulty: None,
                    domains: Vec::new(),
                    weighted_confidence: None,
},
                queued_at: 0,
            })
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("queue full"));
    }

    #[test]
    fn routing_decision_is_local() {
        let local = RoutingDecision {
            category: TaskCategory::Simple,
            tool_needed: None,
            target: RoutingTarget::Local,
            confidence: 0.9,
            reason: "test".into(),
            difficulty: None,
            domains: Vec::new(),
            weighted_confidence: None,
};
        assert!(local.is_local());

        let cloud = RoutingDecision {
            category: TaskCategory::Complex,
            tool_needed: None,
            target: RoutingTarget::Cloud,
            confidence: 0.9,
            reason: "test".into(),
            difficulty: None,
            domains: Vec::new(),
            weighted_confidence: None,
};
        assert!(!cloud.is_local());
    }

    #[test]
    fn classify_coding_request_english() {
        let router = make_router();
        let decision = router.classify("write a Python function to sort a list");
        assert_eq!(decision.target, RoutingTarget::Cloud);
    }

    #[test]
    fn classify_translate_tool() {
        let router = make_router();
        let decision = router.classify("이 문장 번역해줘");
        assert_eq!(decision.category, TaskCategory::Medium);
        assert_eq!(decision.tool_needed, Some("translate".to_string()));
    }
}
