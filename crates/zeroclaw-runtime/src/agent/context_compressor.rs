use std::fmt::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use std::sync::Arc;

use zeroclaw_api::model_provider::{ChatMessage, ModelProvider};
use zeroclaw_memory::traits::Memory;
use zeroclaw_providers::ProviderDispatch;
use zeroclaw_providers::multimodal;

use crate::observability::{Observer, ObserverEvent};

pub use zeroclaw_config::scattered_types::ContextCompressionConfig;

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CompressionResult {
    pub compressed: bool,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub passes_used: u32,
}

// ---------------------------------------------------------------------------
// Probe tiers for unknown model context windows
// ---------------------------------------------------------------------------

const PROBE_TIERS: &[usize] = &[
    2_000_000, 1_000_000, 512_000, 200_000, 128_000, 64_000, 32_000,
];

fn next_probe_tier(current: usize) -> usize {
    PROBE_TIERS
        .iter()
        .copied()
        .find(|&tier| tier < current)
        .unwrap_or(32_000)
}

// ---------------------------------------------------------------------------
// Error message parsing
// ---------------------------------------------------------------------------

/// Try to extract the actual context window limit from a model_provider error message.
pub fn parse_context_limit_from_error(msg: &str) -> Option<usize> {
    // Match patterns like "maximum context length is 128000" or "limit of 200000 tokens"
    // or "context window of 131072" or "available context size (8448 tokens)"
    let re_patterns: &[&str] = &[
        // "maximum context length is 128000"
        r"(?:max(?:imum)?|limit)\s*(?:context\s*)?(?:length|size|window)?\s*(?:is|of|:)?\s*(\d{4,})",
        // "context length is 128000" / "context window of 131072"
        r"context\s*(?:length|size|window)\s*(?:is|of|:)?\s*(\d{4,})",
        // "128000 token context" / "128000 limit"
        r"(\d{4,})\s*(?:tokens?\s*)?(?:context|limit)",
        // "available context size (8448 tokens)"
        r"available context size\s*\(\s*(\d{4,})",
        // "> 128000 maximum context length" (Anthropic-style)
        r">\s*(\d{4,})\s*(?:maximum|max)?\s*(?:context)?\s*(?:length|size|window|tokens?)",
    ];
    let lower = msg.to_lowercase();
    for pattern in re_patterns {
        if let Ok(re) = regex::Regex::new(pattern)
            && let Some(caps) = re.captures(&lower)
            && let Some(m) = caps.get(1)
            && let Ok(limit) = m.as_str().parse::<usize>()
            && (1024..=10_000_000).contains(&limit)
        {
            return Some(limit);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Estimate token count for a message history using ~4 chars/token heuristic
/// with a 1.2x safety margin.
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    let raw: usize = messages
        .iter()
        .map(|m| m.content.len().div_ceil(4) + 4)
        .sum();
    // 1.2x safety margin to account for underestimation
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (raw as f64 * 1.2) as usize
    }
}

// ---------------------------------------------------------------------------
// Summarizer prompt
// ---------------------------------------------------------------------------

const SUMMARIZER_SYSTEM: &str = "\
You are a conversation compaction engine. Summarize the conversation segment below into concise context.

PRESERVE exactly:
- All identifiers (UUIDs, hashes, file paths, URLs, tokens, IPs)
- Actions taken (tool calls, file operations, commands run)
- Key information obtained (data, results, error messages)
- Decisions made and user preferences expressed
- Current task status and unresolved items
- Constraints and requirements mentioned

OMIT:
- Verbose tool output (keep only key results)
- Repeated greetings or filler
- Redundant information already stated

Output concise bullet points. Be thorough but brief.";

// ---------------------------------------------------------------------------
// ContextCompressor
// ---------------------------------------------------------------------------

pub struct ContextCompressor {
    config: ContextCompressionConfig,
    context_window: usize,
    memory: Option<Arc<dyn Memory>>,
    observer: Option<Arc<dyn Observer>>,
}

impl ContextCompressor {
    pub fn new(config: ContextCompressionConfig, context_window: usize) -> Self {
        Self {
            config,
            context_window,
            memory: None,
            observer: None,
        }
    }

    /// Attach a memory handle so compression summaries are persisted before
    /// old messages are discarded. Without this, compressed facts are lost.
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Attach an observer so compression-summary stores emit
    /// `ObserverEvent::MemoryStore` alongside the other agent-loop write
    /// sites. Optional — when not set, stores still happen but are
    /// invisible to OTel/log observers.
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Update the context window size (e.g. after error-driven probing).
    pub fn set_context_window(&mut self, window: usize) {
        self.context_window = window;
    }

    /// Fast-path: trim oversized tool results in non-protected messages.
    /// Returns total characters saved. No LLM call needed.
    fn fast_trim_tool_results(&self, history: &mut [ChatMessage]) -> usize {
        let max = self.config.tool_result_retrim_chars;
        if max == 0 {
            return 0;
        }
        let mut saved = 0;
        let protect_start = self.config.protect_first_n.min(history.len());
        let protect_end = history.len().saturating_sub(self.config.protect_last_n);

        if protect_start >= protect_end {
            return 0;
        }

        for msg in &mut history[protect_start..protect_end] {
            if msg.role != "tool" {
                continue;
            }
            if msg.content.len() <= max {
                continue;
            }
            // Skip exempt tools
            if self
                .config
                .tool_result_trim_exempt
                .iter()
                .any(|t| msg.content.contains(t.as_str()))
            {
                continue;
            }
            // Skip base64 images
            if msg.content.contains("data:image/") {
                continue;
            }
            let original_len = msg.content.len();
            msg.content = crate::agent::history::truncate_tool_message(&msg.content, max);
            saved += original_len - msg.content.len();
        }
        saved
    }

    /// Main entry point. Compresses history in-place if over threshold.
    ///
    /// `temperature` is forwarded verbatim to the summarizer LLM call.
    /// Pass `None` to let the provider decide (required for models that
    /// reject `temperature`, e.g. claude-opus-4-7).
    pub async fn compress_if_needed(
        &self,
        history: &mut Vec<ChatMessage>,
        model_provider: &dyn ModelProvider,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<CompressionResult> {
        if !self.config.enabled {
            let tokens = estimate_tokens(history);
            return Ok(CompressionResult {
                compressed: false,
                tokens_before: tokens,
                tokens_after: tokens,
                passes_used: 0,
            });
        }

        let tokens_before = estimate_tokens(history);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let threshold = (self.context_window as f64 * self.config.threshold_ratio) as usize;

        if tokens_before <= threshold {
            return Ok(CompressionResult {
                compressed: false,
                tokens_before,
                tokens_after: tokens_before,
                passes_used: 0,
            });
        }

        // Fast-trim pass — may resolve overflow without an LLM call
        let chars_saved = self.fast_trim_tool_results(history);
        if chars_saved > 0 {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"chars_saved": chars_saved})),
                "Fast-trim saved chars from old tool results"
            );
            let recheck = estimate_tokens(history);
            if recheck <= threshold {
                return Ok(CompressionResult {
                    compressed: true,
                    tokens_before,
                    tokens_after: recheck,
                    passes_used: 0,
                });
            }
        }

        let mut passes_used = 0;
        for _ in 0..self.config.max_passes {
            let did_compress = self
                .compress_once(history, model_provider, model, temperature)
                .await?;
            if did_compress {
                passes_used += 1;
            }
            if estimate_tokens(history) <= threshold || !did_compress {
                break;
            }
        }

        let tokens_after = estimate_tokens(history);
        Ok(CompressionResult {
            compressed: passes_used > 0,
            tokens_before,
            tokens_after,
            passes_used,
        })
    }

    /// Reactive compression triggered by a context_length_exceeded error.
    /// Parses the actual limit from the error, steps down probe tiers, and re-compresses.
    pub async fn compress_on_error(
        &mut self,
        history: &mut Vec<ChatMessage>,
        model_provider: &dyn ModelProvider,
        model: &str,
        temperature: Option<f64>,
        error_msg: &str,
    ) -> Result<bool> {
        // Try to extract actual limit from error message
        if let Some(limit) = parse_context_limit_from_error(error_msg) {
            self.context_window = limit;
        } else {
            // Step down to next probe tier
            self.context_window = next_probe_tier(self.context_window);
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"context_window": self.context_window})),
            "Context limit adjusted, re-compressing"
        );

        let result = self
            .compress_if_needed(history, model_provider, model, temperature)
            .await?;
        Ok(result.compressed)
    }

    /// Single compression pass: protect head/tail, summarize middle.
    async fn compress_once(
        &self,
        history: &mut Vec<ChatMessage>,
        model_provider: &dyn ModelProvider,
        model: &str,
        temperature: Option<f64>,
    ) -> Result<bool> {
        let n = history.len();
        let protected_total = self.config.protect_first_n + self.config.protect_last_n;
        if n <= protected_total {
            return Ok(false);
        }

        let mut start = self.config.protect_first_n.min(n);
        let mut end = n.saturating_sub(self.config.protect_last_n);

        // Align boundaries to avoid orphaning tool_call/tool_result pairs
        start = align_boundary_forward(history, start);
        end = align_boundary_backward(history, end);

        if start >= end {
            return Ok(false);
        }

        let summary_model = self.config.summary_model.as_deref().unwrap_or(model);
        let preserve_media_markers =
            self.config.summary_model.is_none() && model_provider.supports_vision();

        // Build transcript from the middle section
        let middle = &history[start..end];
        let transcript = build_summarizer_transcript(
            middle,
            self.config.source_max_chars,
            preserve_media_markers,
        );

        if transcript.is_empty() {
            return Ok(false);
        }

        let message_count = end - start;

        let identifier_note = if self.config.identifier_policy == "strict" {
            "\nIMPORTANT: Preserve all identifiers exactly as they appear."
        } else {
            ""
        };

        let user_prompt = format!(
            "Summarize the following conversation history ({message_count} messages) for context preservation. \
             Keep it concise (max 20 bullet points).{identifier_note}\n\n{transcript}"
        );

        // LLM summarization with safety timeout
        let timeout = Duration::from_secs(self.config.timeout_secs);
        let dispatcher = ProviderDispatch::from_ref(model_provider);
        let summary_raw = match tokio::time::timeout(
            timeout,
            dispatcher.chat_with_system(
                Some(SUMMARIZER_SYSTEM),
                &user_prompt,
                summary_model,
                temperature,
            ),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Summarization LLM call failed, using transcript truncation"
                );
                truncate_chars(&transcript, self.config.summary_max_chars)
            }
            Err(_) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "Summarization timed out after {}s, using transcript truncation",
                        self.config.timeout_secs
                    )
                );
                truncate_chars(&transcript, self.config.summary_max_chars)
            }
        };

        let summary = truncate_chars(&summary_raw, self.config.summary_max_chars);

        // Persist the compression summary to memory before discarding old messages.
        // This ensures facts from compressed turns remain retrievable via memory recall.
        if let Some(ref memory) = self.memory {
            let facts_key = format!("compressed_context_{}", uuid::Uuid::new_v4());
            let category = zeroclaw_memory::traits::MemoryCategory::Daily;
            let store_start = Instant::now();
            let store_result = memory
                .store(&facts_key, &summary, category.clone(), None)
                .await;
            let store_duration = store_start.elapsed();
            let success = store_result.is_ok();
            match &store_result {
                Ok(_) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"message_count": message_count})),
                        "Saved compression summary to memory before discarding messages"
                    );
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Failed to save compression summary to memory"
                    );
                }
            }
            if let Some(ref observer) = self.observer {
                observer.record_event(&ObserverEvent::MemoryStore {
                    category: category.to_string(),
                    backend: memory.name().to_string(),
                    duration: store_duration,
                    success,
                });
            }
        }

        // Splice: head + [SUMMARY] + tail
        let summary_msg = build_summary_message(&history[start..end], &summary, message_count);
        history.splice(start..end, std::iter::once(summary_msg));

        // Repair orphaned tool pairs
        let tool_pairs_removed = repair_tool_pairs(history);

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "messages_summarized": message_count,
                    "summary_chars": summary.len(),
                    "tool_pairs_removed": tool_pairs_removed,
                    "protect_first_n": self.config.protect_first_n,
                    "protect_last_n": self.config.protect_last_n,
                })),
            "context_compressor: middle of conversation replaced with a \
             text summary. The model loses structural tool_use/tool_result \
             pairs from this range. If this fires mid-turn the model can \
             act like it just woke up. Raise protect_last_n / \
             protect_first_n, or raise max_context_tokens, or lower \
             threshold_ratio carefully."
        );

        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Boundary alignment
// ---------------------------------------------------------------------------

/// Move boundary forward past any orphaned tool results at the start.
fn align_boundary_forward(messages: &[ChatMessage], idx: usize) -> usize {
    let mut i = idx;
    while i < messages.len() && messages[i].role == "tool" {
        i += 1;
    }
    i
}

/// Move the tail boundary backward past any orphan-creating split, and
/// past the assistant + user pair that initiated the current turn.
///
/// The goal is "the protected tail starts on a turn boundary, not in
/// the middle of a turn." Without this the compressor summarises the
/// user's question while leaving the assistant's response + tool work
/// in the tail, and the model re-enters the loop seeing dispatched
/// tools with no prompt — looks exactly like "the model woke up
/// mid-action."
///
/// One-shot, not iterative: this aligns the *tail edge* to the nearest
/// preceding turn boundary, it does NOT keep eating preceding turns.
/// The middle still gets compressed; only the boundary moves.
///
/// Returns the new `end` index such that `middle = messages[start..end]`
/// and `tail = messages[end..]`.
fn align_boundary_backward(messages: &[ChatMessage], idx: usize) -> usize {
    let mut i = idx;

    // Step past any leading `tool` messages — their owning assistant
    // is earlier and must travel with them into the protected tail.
    while i > 0 && messages[i - 1].role == "tool" {
        i -= 1;
    }

    // Step past trailing `assistant` messages — both the tool-dispatching
    // one and any preamble assistant immediately preceding it. The
    // original code only stepped past assistants carrying `tool_calls`,
    // which left preamble text in the middle.
    while i > 0 && messages[i - 1].role == "assistant" {
        i -= 1;
    }

    // Step past exactly one user message — the one that initiated this
    // turn — so the turn is protected atomically. Do NOT loop back
    // further: that would eat the entire conversation and produce an
    // empty middle.
    if i > 0 && messages[i - 1].role == "user" {
        i -= 1;
    }

    i
}

// ---------------------------------------------------------------------------
// Tool pair repair
// ---------------------------------------------------------------------------

/// Remove orphaned tool_results and add stubs for orphaned tool_calls.
///
/// After compression, some tool results may reference tool_calls that were
/// summarized away, and vice versa. This function cleans up the history
/// so every tool_result has a matching assistant message and every
/// tool_call-bearing assistant message has results.
fn repair_tool_pairs(messages: &mut Vec<ChatMessage>) -> usize {
    let mut removed = 0;
    // Heuristic: tool messages whose content references a call ID that no longer
    // exists in any assistant message should be removed. Since ChatMessage is a
    // simple role+content struct (no structured tool_call_id field), we use a
    // simpler approach: remove any "tool" message that immediately follows the
    // [CONTEXT SUMMARY] message (it's orphaned by definition).
    let mut i = 0;
    while i < messages.len() {
        if messages[i].content.contains("[CONTEXT SUMMARY") {
            // Remove any immediately following orphaned tool results
            while i + 1 < messages.len() && messages[i + 1].role == "tool" {
                messages.remove(i + 1);
                removed += 1;
            }
        }
        i += 1;
    }

    // Also check for tool results at the very start (after system prompt) that
    // are orphaned because their assistant message was compressed.
    let start = if messages.first().is_some_and(|m| m.role == "system") {
        1
    } else {
        0
    };
    while start < messages.len() && messages[start].role == "tool" {
        messages.remove(start);
        removed += 1;
    }
    removed
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_full_transcript(messages: &[ChatMessage]) -> String {
    let mut transcript = String::new();
    for msg in messages {
        let role = msg.role.to_uppercase();
        let _ = writeln!(transcript, "{role}: {}", msg.content.trim());
    }
    transcript
}

fn build_summarizer_transcript(
    messages: &[ChatMessage],
    max_chars: usize,
    preserve_media_markers: bool,
) -> String {
    let transcript = build_full_transcript(messages);
    if preserve_media_markers {
        // Vision-capable summarizer can read media markers; preserve them so
        // visual content is reflected in the summary (per #6189 contract).
        return truncate_owned_if_needed(transcript, max_chars);
    }

    // Non-vision summarizer cannot consume media markers. Strip ALL inbound
    // attachment-kind markers (IMAGE, PHOTO, DOCUMENT, FILE, VIDEO, VOICE,
    // AUDIO — case-insensitive) instead of just `[IMAGE:...]`, otherwise a
    // local filesystem path can leak into the auxiliary `chat_with_system`
    // payload and the upstream API rejects it as a malformed `image_url.url`.
    truncate_owned_if_needed(multimodal::strip_media_markers(&transcript), max_chars)
}

fn truncate_owned_if_needed(s: String, max: usize) -> String {
    if s.len() > max {
        truncate_chars(&s, max)
    } else {
        s
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Find a safe char boundary
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = s[..end].to_string();
    result.push_str("...");
    result
}

/// Construct the synthesized assistant message that replaces a compressed
/// range. When the compressed range contains an assistant turn with
/// `reasoning_content` (a thinking-mode response from providers like
/// DeepSeek V4), embed the most recent such payload in the summary as a
/// JSON-encoded `{content, reasoning_content}` body — matching the shape
/// `build_native_assistant_history` already produces — so the next request
/// to the provider passes its reasoning round-trip check. See #6269.
fn build_summary_message(
    compressed: &[ChatMessage],
    summary: &str,
    message_count: usize,
) -> ChatMessage {
    let summary_text = format!(
        "[CONTEXT SUMMARY \u{2014} {message_count} earlier messages compressed]\n\n{summary}"
    );

    let last_reasoning = compressed
        .iter()
        .rev()
        .filter(|m| m.role == "assistant")
        .find_map(|m| {
            serde_json::from_str::<serde_json::Value>(&m.content)
                .ok()
                .and_then(|v| {
                    v.get("reasoning_content")
                        .and_then(|rc| rc.as_str().map(ToString::to_string))
                })
        });

    if let Some(rc) = last_reasoning {
        let payload = serde_json::json!({
            "content": summary_text,
            "reasoning_content": rc,
        });
        ChatMessage::assistant(payload.to_string())
    } else {
        ChatMessage::assistant(summary_text)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    struct CaptureSummarizerModelProvider {
        supports_vision: bool,
        seen_messages: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ModelProvider for CaptureSummarizerModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            self.seen_messages.lock().push(message.to_string());
            Ok("summary".to_string())
        }

        async fn chat(
            &self,
            _request: zeroclaw_api::model_provider::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_api::model_provider::ChatResponse> {
            unreachable!("context compressor uses chat_with_system")
        }

        fn supports_vision(&self) -> bool {
            self.supports_vision
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for CaptureSummarizerModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "CaptureSummarizerModelProvider"
        }
    }

    #[test]
    fn test_estimate_tokens() {
        let messages = vec![msg("user", "hello world")]; // 11 chars
        let tokens = estimate_tokens(&messages);
        // 11/4 ceil = 3, +4 framing = 7, *1.2 = 8.4 -> 8
        assert!(tokens > 0);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn test_parse_context_limit_anthropic() {
        let msg = "prompt is too long: 150000 tokens > 128000 maximum context length";
        assert_eq!(parse_context_limit_from_error(msg), Some(128_000));
    }

    #[test]
    fn test_parse_context_limit_openai() {
        let msg = "This model's maximum context length is 128000 tokens. However, your messages resulted in 150000 tokens.";
        assert_eq!(parse_context_limit_from_error(msg), Some(128_000));
    }

    #[test]
    fn test_parse_context_limit_llamacpp() {
        let msg = "request (8968 tokens) exceeds the available context size (8448 tokens)";
        assert_eq!(parse_context_limit_from_error(msg), Some(8448));
    }

    #[test]
    fn test_parse_context_limit_none() {
        assert_eq!(parse_context_limit_from_error("some random error"), None);
    }

    #[test]
    fn test_parse_context_limit_rejects_small() {
        let msg = "limit is 100 tokens";
        assert_eq!(parse_context_limit_from_error(msg), None); // < 1024
    }

    #[test]
    fn test_next_probe_tier() {
        assert_eq!(next_probe_tier(2_000_001), 2_000_000);
        assert_eq!(next_probe_tier(2_000_000), 1_000_000);
        assert_eq!(next_probe_tier(200_000), 128_000);
        assert_eq!(next_probe_tier(64_000), 32_000);
        assert_eq!(next_probe_tier(32_000), 32_000); // floor
        assert_eq!(next_probe_tier(10_000), 32_000); // below all tiers
    }

    #[test]
    fn test_align_boundary_forward_skips_tool() {
        let messages = vec![
            msg("system", "sys"),
            msg("user", "q"),
            msg("tool", "result1"),
            msg("tool", "result2"),
            msg("user", "next"),
        ];
        // Starting at index 2 (tool), should skip to index 4
        assert_eq!(align_boundary_forward(&messages, 2), 4);
    }

    #[test]
    fn test_align_boundary_forward_noop() {
        let messages = vec![
            msg("system", "sys"),
            msg("user", "q"),
            msg("assistant", "a"),
        ];
        assert_eq!(align_boundary_forward(&messages, 1), 1);
    }

    #[test]
    fn test_repair_tool_pairs_removes_orphaned() {
        let mut messages = vec![
            msg("system", "sys"),
            msg(
                "assistant",
                "[CONTEXT SUMMARY — 5 earlier messages compressed]\nstuff",
            ),
            msg("tool", "orphaned result"),
            msg("user", "next question"),
        ];
        repair_tool_pairs(&mut messages);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn test_repair_tool_pairs_no_false_positives() {
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "q"),
            msg("assistant", "calling tool"),
            msg("tool", "result"),
            msg("user", "thanks"),
        ];
        repair_tool_pairs(&mut messages);
        assert_eq!(messages.len(), 5); // no change
    }

    /// Regression test for the root-cause #5813 fix: when the tail
    /// boundary lands on an assistant with `tool_calls`, the function
    /// must back up past it so the assistant travels with its
    /// `tool_result` blocks into the protected tail. Otherwise the
    /// assistant gets summarized while its results survive, creating an
    /// orphan and producing the 400 "unexpected tool_use_id" failure.
    #[test]
    fn test_align_boundary_backward_backs_up_past_tool_call_assistant() {
        let messages = vec![
            msg("system", "sys"),
            msg("user", "q1"),
            msg("assistant", "old reply 1"),
            msg("user", "q2"),
            msg(
                "assistant",
                r#"{"content":null,"tool_calls":[{"id":"toolu_X","name":"shell","arguments":"{}"}]}"#,
            ),
            msg("tool", r#"{"tool_call_id":"toolu_X","content":"result"}"#),
            msg("user", "follow-up"),
        ];
        // Initial boundary lands on the assistant(tool_calls) at index 4.
        // The function must back up past it so the pair stays in the tail.
        let aligned = align_boundary_backward(&messages, 4);
        assert!(
            aligned < 4,
            "boundary should retreat past assistant(tool_calls) at idx 4, got {aligned}"
        );
    }

    #[test]
    fn test_align_boundary_backward_protects_whole_turn() {
        // Boundary alignment must back up past the assistant AND the
        // user that initiated the turn so the protected tail contains
        // complete user→assistant turns. Previously this returned 2,
        // splitting `[U:q, A:plain] | [U:next]` and leaving the model
        // looking at a tail that starts with a user message but missing
        // the prior assistant's framing.
        let messages = vec![
            msg("system", "sys"),
            msg("user", "q"),
            msg("assistant", "plain text reply"),
            msg("user", "next"),
        ];
        // Tail starts at `[A: plain]`. Function steps back past the
        // assistant + its initiating user, landing on 1 (the protected
        // tail is `[U:q, A:plain, U:next]`).
        assert_eq!(align_boundary_backward(&messages, 2), 1);
    }

    #[test]
    fn test_build_transcript() {
        let messages = vec![msg("user", "hello"), msg("assistant", "hi there")];
        let t = build_full_transcript(&messages);
        assert!(t.contains("USER: hello"));
        assert!(t.contains("ASSISTANT: hi there"));
    }

    #[test]
    fn test_build_summarizer_transcript_strips_all_attachment_kinds_for_non_vision_provider() {
        // The non-vision summarizer branch must strip every inbound
        // attachment-kind alias the channel parsers can emit, not just
        // `[IMAGE:]`. Mirrors `ATTACHMENT_KINDS` in
        // `crates/zeroclaw-channels/src/util.rs`. Regression: a `[PHOTO:]`
        // or `[DOCUMENT:]` marker still leaking through would surface a
        // local filesystem path in the auxiliary `chat_with_system` payload
        // and the upstream API would reject it.
        let messages = vec![msg(
            "user",
            "Take a look at [IMAGE:/a.jpg] [PHOTO:/b.jpg] [DOCUMENT:/c.pdf] \
             [FILE:/d.zip] [VIDEO:/e.mp4] [VOICE:/f.ogg] [AUDIO:/g.wav] please",
        )];
        let transcript = build_summarizer_transcript(&messages, 10_000, false);
        for prefix in [
            "[IMAGE:",
            "[PHOTO:",
            "[DOCUMENT:",
            "[FILE:",
            "[VIDEO:",
            "[VOICE:",
            "[AUDIO:",
        ] {
            assert!(
                !transcript.contains(prefix),
                "non-vision transcript should not contain raw {prefix} marker: {transcript}"
            );
        }
        assert!(
            transcript.contains("[media attachment]"),
            "non-vision transcript should contain placeholder: {transcript}"
        );
        assert!(transcript.contains("Take a look at"));
        assert!(transcript.contains("please"));
    }

    #[test]
    fn test_build_summarizer_transcript_strips_media_markers_before_truncation() {
        let long_path = format!(
            "/private/tmp/zeroclaw/signal_inbound/{}",
            "nested-directory/".repeat(12)
        );
        let messages = vec![msg(
            "user",
            &format!("Please summarize [IMAGE:{long_path}photo.png] after text"),
        )];

        let transcript = build_summarizer_transcript(&messages, 64, false);

        assert!(
            !transcript.contains("[IMAGE:"),
            "non-vision transcript should not retain a split image marker: {transcript}"
        );
        assert!(
            !transcript.contains("/private/tmp"),
            "non-vision transcript should not leak local path fragments: {transcript}"
        );
        assert!(
            transcript.contains("[media attachment]"),
            "non-vision transcript should preserve an attachment placeholder: {transcript}"
        );
    }

    #[test]
    fn test_build_transcript_truncates() {
        let messages = vec![msg("user", &"x".repeat(1000))];
        let t = truncate_owned_if_needed(build_full_transcript(&messages), 100);
        assert!(t.len() <= 103); // 100 + "..."
    }

    #[test]
    fn test_build_summarizer_transcript_strips_image_markers_for_non_vision_provider() {
        let messages = vec![msg(
            "user",
            "Describe this photo [IMAGE:/tmp/test.png]\nKeep the caption",
        )];
        let transcript = build_summarizer_transcript(&messages, 10_000, false);
        assert!(!transcript.contains("[IMAGE:"));
        assert!(transcript.contains("Describe this photo"));
        assert!(transcript.contains("Keep the caption"));
    }

    #[test]
    fn test_build_summarizer_transcript_keeps_image_markers_for_vision_provider() {
        let messages = vec![msg("user", "Describe this photo [IMAGE:/tmp/test.png]")];
        let transcript = build_summarizer_transcript(&messages, 10_000, true);
        assert!(transcript.contains("[IMAGE:/tmp/test.png]"));
    }

    #[test]
    fn test_truncate_chars() {
        assert_eq!(truncate_chars("hello world", 5), "hello...");
        assert_eq!(truncate_chars("hi", 10), "hi");
    }

    #[test]
    fn test_config_defaults() {
        let config = ContextCompressionConfig::default();
        assert!(config.enabled);
        assert!((config.threshold_ratio - 0.50).abs() < f64::EPSILON);
        assert_eq!(config.protect_first_n, 3);
        assert_eq!(config.protect_last_n, 4);
        assert_eq!(config.max_passes, 3);
        assert_eq!(config.summary_max_chars, 4_000);
        assert_eq!(config.source_max_chars, 50_000);
        assert_eq!(config.timeout_secs, 60);
        assert!(config.summary_model.is_none());
        assert_eq!(config.identifier_policy, "strict");
    }

    #[test]
    fn test_config_serde_defaults() {
        let json = "{}";
        let config: ContextCompressionConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.protect_first_n, 3);
        assert_eq!(config.max_passes, 3);
    }

    #[test]
    fn test_config_serde_override() {
        let json = r#"{"enabled": false, "protect_first_n": 5, "max_passes": 1}"#;
        let config: ContextCompressionConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.protect_first_n, 5);
        assert_eq!(config.max_passes, 1);
    }

    #[tokio::test]
    async fn compress_if_needed_strips_image_markers_before_non_vision_summarization() {
        let config = ContextCompressionConfig {
            protect_first_n: 1,
            protect_last_n: 1,
            threshold_ratio: 0.01,
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 64);
        let model_provider = CaptureSummarizerModelProvider {
            supports_vision: false,
            seen_messages: Mutex::new(Vec::new()),
        };
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "First question"),
            msg("assistant", "First answer"),
            msg("user", "Middle question [IMAGE:/tmp/example.png]"),
            msg("assistant", "Middle answer about the image"),
            msg("user", "Another middle question"),
            msg("assistant", "Another middle answer"),
            msg("user", "Newest question"),
        ];

        let result = compressor
            .compress_if_needed(&mut history, &model_provider, "model", None)
            .await
            .expect("compression should succeed");

        assert!(result.compressed);
        let seen = model_provider.seen_messages.lock();
        let prompt = seen.last().expect("summarizer should be invoked");
        assert!(!prompt.contains("[IMAGE:"));
        assert!(!prompt.contains("/tmp/example.png"));
    }

    #[tokio::test]
    async fn compress_if_needed_strips_image_markers_when_summary_model_overrides() {
        let config = ContextCompressionConfig {
            protect_first_n: 1,
            protect_last_n: 1,
            threshold_ratio: 0.01,
            summary_model: Some("text-summary-model".to_string()),
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 64);
        let model_provider = CaptureSummarizerModelProvider {
            supports_vision: true,
            seen_messages: Mutex::new(Vec::new()),
        };
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "First question"),
            msg("assistant", "First answer"),
            msg("user", "Middle question [IMAGE:/tmp/summary-override.png]"),
            msg("assistant", "Middle answer about the image"),
            msg("user", "Another middle question"),
            msg("assistant", "Another middle answer"),
            msg("user", "Newest question"),
        ];

        let result = compressor
            .compress_if_needed(&mut history, &model_provider, "default-vision-model", None)
            .await
            .expect("compression should succeed");

        assert!(result.compressed);
        let seen = model_provider.seen_messages.lock();
        let prompt = seen.last().expect("summarizer should be invoked");
        assert!(!prompt.contains("[IMAGE:"));
        assert!(!prompt.contains("/tmp/summary-override.png"));
    }

    // ── fast_trim_tool_results tests ────────────────────────────────

    #[test]
    fn test_fast_trim_protects_first_and_last_n() {
        let config = ContextCompressionConfig {
            protect_first_n: 2,
            protect_last_n: 2,
            tool_result_retrim_chars: 100,
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 128_000);
        let big = "x".repeat(5_000);
        let mut history = vec![
            msg("system", "sys"),
            msg("tool", &big), // index 1 — protected (first 2)
            msg("user", "q"),
            msg("tool", &big),   // index 3 — trimmable
            msg("user", "next"), // index 4 — protected (last 2)
            msg("tool", &big),   // index 5 — protected (last 2)
        ];
        let saved = compressor.fast_trim_tool_results(&mut history);
        assert!(saved > 0);
        // Protected messages unchanged
        assert_eq!(history[1].content.len(), 5_000);
        assert_eq!(history[5].content.len(), 5_000);
        // Trimmable message was trimmed
        assert!(history[3].content.len() <= 200); // 100 + marker overhead
    }

    #[test]
    fn test_fast_trim_skips_images() {
        let config = ContextCompressionConfig {
            protect_first_n: 0,
            protect_last_n: 0,
            tool_result_retrim_chars: 100,
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 128_000);
        let img = format!("data:image/{}", "x".repeat(5_000));
        let mut history = vec![msg("tool", &img)];
        let saved = compressor.fast_trim_tool_results(&mut history);
        assert_eq!(saved, 0);
        assert!(history[0].content.len() > 5_000);
    }

    #[test]
    fn test_fast_trim_skips_exempt_tools() {
        let config = ContextCompressionConfig {
            protect_first_n: 0,
            protect_last_n: 0,
            tool_result_retrim_chars: 100,
            tool_result_trim_exempt: vec!["KEEPME".to_string()],
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 128_000);
        let content = format!("KEEPME {}", "x".repeat(5_000));
        let mut history = vec![msg("tool", &content)];
        let saved = compressor.fast_trim_tool_results(&mut history);
        assert_eq!(saved, 0);
    }

    #[test]
    fn test_fast_trim_skips_small_results() {
        let config = ContextCompressionConfig {
            protect_first_n: 0,
            protect_last_n: 0,
            tool_result_retrim_chars: 2_000,
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 128_000);
        let mut history = vec![msg("tool", "small result")];
        let saved = compressor.fast_trim_tool_results(&mut history);
        assert_eq!(saved, 0);
    }

    #[test]
    fn test_fast_trim_skips_non_tool_messages() {
        let config = ContextCompressionConfig {
            protect_first_n: 0,
            protect_last_n: 0,
            tool_result_retrim_chars: 100,
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 128_000);
        let big = "x".repeat(5_000);
        let mut history = vec![msg("user", &big), msg("assistant", &big)];
        let saved = compressor.fast_trim_tool_results(&mut history);
        assert_eq!(saved, 0);
    }

    #[test]
    fn test_fast_trim_config_defaults() {
        let config = ContextCompressionConfig::default();
        assert_eq!(config.tool_result_retrim_chars, 2_000);
        assert!(config.tool_result_trim_exempt.is_empty());
    }

    #[test]
    fn test_fast_trim_disabled_when_zero() {
        let config = ContextCompressionConfig {
            protect_first_n: 0,
            protect_last_n: 0,
            tool_result_retrim_chars: 0,
            ..Default::default()
        };
        let compressor = ContextCompressor::new(config, 128_000);
        let big = "x".repeat(5_000);
        let mut history = vec![msg("tool", &big)];
        let saved = compressor.fast_trim_tool_results(&mut history);
        assert_eq!(saved, 0);
    }

    /// When the compressed range has no thinking-mode reasoning_content,
    /// the synthesized summary is plain text — same as before #6269.
    #[test]
    fn build_summary_message_uses_plain_text_when_no_reasoning() {
        let compressed = vec![
            msg("user", "what's the weather"),
            msg("assistant", "it's sunny"),
        ];
        let out = build_summary_message(&compressed, "weather chat", 2);
        assert_eq!(out.role, "assistant");
        assert!(out.content.starts_with("[CONTEXT SUMMARY"));
        assert!(out.content.contains("weather chat"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&out.content).is_err(),
            "plain-text summary must not parse as JSON"
        );
    }

    /// Regression test for #6269 — when an assistant message in the
    /// compressed range carries `reasoning_content` (thinking-mode replay
    /// payload), the synthesized summary preserves it via JSON-encoded
    /// content matching `build_native_assistant_history`'s shape.
    /// Without this, providers that require reasoning round-trip
    /// (DeepSeek V4 thinking) reject every post-compression request.
    #[test]
    fn build_summary_message_preserves_reasoning_content_when_present() {
        let assistant_with_reasoning = serde_json::json!({
            "content": "let me look",
            "reasoning_content": "user wants weather; need to check",
        })
        .to_string();
        let compressed = vec![
            msg("user", "what's the weather"),
            msg("assistant", &assistant_with_reasoning),
        ];

        let out = build_summary_message(&compressed, "weather chat", 2);
        assert_eq!(out.role, "assistant");
        let parsed: serde_json::Value = serde_json::from_str(&out.content)
            .expect("summary must be JSON when reasoning_content is preserved");
        assert!(
            parsed["content"]
                .as_str()
                .is_some_and(|s| s.starts_with("[CONTEXT SUMMARY")),
            "summary text belongs in `content`",
        );
        assert_eq!(
            parsed["reasoning_content"].as_str(),
            Some("user wants weather; need to check"),
            "must carry reasoning_content from the most recent compressed assistant turn",
        );
    }

    /// When multiple compressed assistant turns have reasoning_content,
    /// the most recent one survives — this matches DeepSeek's protocol
    /// expectation that the *immediately prior* assistant turn's
    /// reasoning is what gets replayed.
    #[test]
    fn build_summary_message_picks_last_reasoning_content() {
        let earlier = serde_json::json!({
            "content": "first answer",
            "reasoning_content": "EARLIER reasoning",
        })
        .to_string();
        let later = serde_json::json!({
            "content": "second answer",
            "reasoning_content": "LATER reasoning",
        })
        .to_string();
        let compressed = vec![
            msg("user", "q1"),
            msg("assistant", &earlier),
            msg("user", "q2"),
            msg("assistant", &later),
        ];

        let out = build_summary_message(&compressed, "two-turn chat", 4);
        let parsed: serde_json::Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(
            parsed["reasoning_content"].as_str(),
            Some("LATER reasoning"),
            "must pick the most recent reasoning_content, not the earliest",
        );
    }
}
