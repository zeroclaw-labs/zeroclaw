use crate::memory::{Memory, MemoryCategory};
use crate::providers::{ChatMessage, Provider};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::fmt::Write;

/// Keep this many most-recent non-system messages after compaction.
const COMPACTION_KEEP_RECENT_MESSAGES: usize = 20;

/// Safety cap for compaction source transcript passed to the summarizer.
const COMPACTION_MAX_SOURCE_CHARS: usize = 12_000;

/// Max characters retained in stored compaction summary.
const COMPACTION_MAX_SUMMARY_CHARS: usize = 2_000;

/// Safety cap for durable facts extracted during pre-compaction flush.
const COMPACTION_MAX_FLUSH_FACTS: usize = 8;

/// Fraction of `context_window_tokens` at which mid-loop compaction fires.
/// Set to 70% to leave headroom for the system prompt, tool specs, and the
/// next LLM response.
const COMPACTION_TRIGGER_RATIO: f64 = 0.70;

/// Derive the mid-loop compaction threshold from the configured context window.
pub(super) fn compaction_token_threshold(context_window_tokens: usize) -> usize {
    (context_window_tokens as f64 * COMPACTION_TRIGGER_RATIO) as usize
}

/// How many recent non-system messages to keep when the mid-loop trim fires.
/// Larger than `COMPACTION_KEEP_RECENT_MESSAGES` to preserve more working
/// context during an active tool sequence.
pub(super) const COMPACTION_KEEP_RECENT_MESSAGES_FOR_TRIM: usize = 30;

/// Rough chars-to-tokens factor: 1 token ≈ 3 chars + 4 overhead per message.
/// Matches the estimation used in `channels/mod.rs`.
pub(super) fn estimated_history_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .map(|m| (m.content.chars().count().saturating_add(2) / 3).saturating_add(4))
        .sum()
}

/// Strip `reasoning_content` from all assistant messages except the last one.
/// Providers like Anthropic and OpenAI filter stale reasoning server-side,
/// but proxy stacks (e.g. LiteLLM → llama.cpp) pass everything through.
pub(super) fn strip_prior_reasoning(messages: &mut [ChatMessage]) {
    let last_assistant_idx = messages.iter().rposition(|m| m.role == "assistant");
    for (i, msg) in messages.iter_mut().enumerate() {
        if msg.role != "assistant" || Some(i) == last_assistant_idx {
            continue;
        }
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
            if value.get("reasoning_content").is_some() {
                value.as_object_mut().unwrap().remove("reasoning_content");
                msg.content = value.to_string();
            }
        }
    }
}

/// Trim conversation history to prevent unbounded growth.
/// Preserves the system prompt (first message if role=system) and the most recent messages.
pub(super) fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    // Nothing to trim if within limit
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len() - 1
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return;
    }

    let start = if has_system { 1 } else { 0 };
    let mut trim_end = start + (non_system_count - max_history);
    // Never keep a leading `role=tool` at the trim boundary. Tool-message runs
    // must remain attached to their preceding assistant(tool_calls) message.
    while trim_end < history.len() && history[trim_end].role == "tool" {
        trim_end += 1;
    }
    history.drain(start..trim_end);
}

/// Maximum characters retained per tool-result message when building the
/// compaction transcript. A single large tool result should not crowd out all
/// other messages, so we cap each one individually before joining.
const COMPACTION_MAX_TOOL_RESULT_CHARS: usize = 2_000;

/// Truncate a tool message while preserving the `{"tool_call_id": …, "content": …}`
/// JSON envelope so that downstream consumers can still correlate the result with
/// its originating call.
///
/// When `msg_content` is a JSON object that contains a `tool_call_id` key, only
/// the inner `content` string is truncated; the surrounding envelope is kept
/// intact and re-serialised. For any other content format the input is truncated
/// directly with [`truncate_with_ellipsis`].
///
/// Passing `max_chars == 0` is treated as "no limit" and returns the input as-is.
pub(crate) fn truncate_tool_message(msg_content: &str, max_chars: usize) -> String {
    if max_chars == 0 || msg_content.len() <= max_chars {
        return msg_content.to_string();
    }
    if let Ok(mut obj) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(msg_content)
    {
        if obj.contains_key("tool_call_id") {
            if let Some(serde_json::Value::String(inner)) = obj.get("content") {
                let truncated = truncate_with_ellipsis(inner, max_chars);
                obj.insert(
                    "content".to_string(),
                    serde_json::Value::String(truncated),
                );
                return serde_json::to_string(&obj)
                    .unwrap_or_else(|_| msg_content.to_string());
            }
        }
    }
    truncate_with_ellipsis(msg_content, max_chars)
}

pub(crate) fn build_compaction_transcript(messages: &[ChatMessage]) -> String {
    let mut transcript = String::new();
    for msg in messages {
        let role = msg.role.to_uppercase();
        // Cap individual tool results so one large result cannot crowd out all
        // other messages in the compaction transcript.
        let content = if msg.role == "tool" {
            truncate_tool_message(msg.content.trim(), COMPACTION_MAX_TOOL_RESULT_CHARS)
        } else {
            msg.content.trim().to_string()
        };
        let _ = writeln!(transcript, "{role}: {}", content.trim());
    }

    if transcript.chars().count() > COMPACTION_MAX_SOURCE_CHARS {
        truncate_with_ellipsis(&transcript, COMPACTION_MAX_SOURCE_CHARS)
    } else {
        transcript
    }
}

pub(super) fn apply_compaction_summary(
    history: &mut Vec<ChatMessage>,
    start: usize,
    compact_end: usize,
    summary: &str,
) {
    let summary_msg = ChatMessage::assistant(format!("[Compaction summary]\n{}", summary.trim()));
    history.splice(start..compact_end, std::iter::once(summary_msg));
}

pub(super) async fn auto_compact_history(
    history: &mut Vec<ChatMessage>,
    provider: &dyn Provider,
    model: &str,
    max_history: usize,
    hooks: Option<&crate::hooks::HookRunner>,
    memory: Option<&dyn Memory>,
) -> Result<bool> {
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len().saturating_sub(1)
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return Ok(false);
    }

    let start = if has_system { 1 } else { 0 };
    let keep_recent = COMPACTION_KEEP_RECENT_MESSAGES.min(non_system_count);
    let compact_count = non_system_count.saturating_sub(keep_recent);
    if compact_count == 0 {
        return Ok(false);
    }

    let mut compact_end = start + compact_count;
    // Do not split assistant(tool_calls) -> tool runs across compaction boundary.
    while compact_end < history.len() && history[compact_end].role == "tool" {
        compact_end += 1;
    }
    let to_compact: Vec<ChatMessage> = history[start..compact_end].to_vec();
    let to_compact = if let Some(hooks) = hooks {
        match hooks.run_before_compaction(to_compact).await {
            crate::hooks::HookResult::Continue(messages) => messages,
            crate::hooks::HookResult::Cancel(reason) => {
                tracing::info!(%reason, "history compaction cancelled by hook");
                return Ok(false);
            }
        }
    } else {
        to_compact
    };
    let transcript = build_compaction_transcript(&to_compact);

    // ── Pre-compaction memory flush ──────────────────────────────────
    // Before discarding old messages, ask the LLM to extract durable
    // facts and store them as Core memories so they survive compaction.
    if let Some(mem) = memory {
        flush_durable_facts(provider, model, &transcript, mem).await;
    }

    let summarizer_system = "You are a conversation compaction engine. Summarize older chat history into concise context for future turns. Preserve: user preferences, commitments, decisions, unresolved tasks, key facts. Omit: filler, repeated chit-chat, verbose tool logs. Output plain text bullet points only.";

    let summarizer_user = format!(
        "Summarize the following conversation history for context preservation. Keep it short (max 12 bullet points).\n\n{}",
        transcript
    );

    let summary_raw = provider
        .chat_with_system(Some(summarizer_system), &summarizer_user, model, 0.2)
        .await
        .unwrap_or_else(|_| {
            // Fallback to deterministic local truncation when summarization fails.
            truncate_with_ellipsis(&transcript, COMPACTION_MAX_SUMMARY_CHARS)
        });

    let summary = truncate_with_ellipsis(&summary_raw, COMPACTION_MAX_SUMMARY_CHARS);
    let summary = if let Some(hooks) = hooks {
        match hooks.run_after_compaction(summary).await {
            crate::hooks::HookResult::Continue(next_summary) => next_summary,
            crate::hooks::HookResult::Cancel(reason) => {
                tracing::info!(%reason, "post-compaction summary cancelled by hook");
                return Ok(false);
            }
        }
    } else {
        summary
    };
    apply_compaction_summary(history, start, compact_end, &summary);

    Ok(true)
}

/// Extract durable facts from a conversation transcript and store them as
/// `Core` memories. Called before compaction discards old messages, and
/// by the `/checkpoint` command to persist facts on demand.
///
/// Best-effort: failures are logged but never block compaction.
pub(crate) async fn flush_durable_facts(
    provider: &dyn Provider,
    model: &str,
    transcript: &str,
    memory: &dyn Memory,
) {
    const FLUSH_SYSTEM: &str = "\
You extract durable facts from a conversation that is about to be compacted. \
Output ONLY facts worth remembering long-term — user preferences, project decisions, \
technical constraints, commitments, or important discoveries. \
Output one fact per line, prefixed with a short key in brackets. \
Example:\n\
[preferred_language] User prefers Rust over Go\n\
[db_choice] Project uses PostgreSQL 16\n\
If there are no durable facts, output exactly: NONE";

    let flush_user = format!(
        "Extract durable facts from this conversation (max 8 facts):\n\n{}",
        transcript
    );

    let response = match provider
        .chat_with_system(Some(FLUSH_SYSTEM), &flush_user, model, 0.2)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Pre-compaction memory flush failed: {e}");
            return;
        }
    };

    if response.trim().eq_ignore_ascii_case("NONE") || response.trim().is_empty() {
        return;
    }

    let mut stored = 0usize;
    for line in response.lines() {
        if stored >= COMPACTION_MAX_FLUSH_FACTS {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Parse "[key] content" format
        if let Some((key, content)) = parse_fact_line(line) {
            let prefixed_key = format!("compaction_fact_{key}");
            if let Err(e) = memory
                .store(&prefixed_key, content, MemoryCategory::Core, None)
                .await
            {
                tracing::warn!("Failed to store compaction fact '{prefixed_key}': {e}");
            } else {
                stored += 1;
            }
        }
    }
    if stored > 0 {
        tracing::info!("Pre-compaction flush: stored {stored} durable fact(s) to Core memory");
    }
}

/// Parse a `[key] content` line from the fact extraction output.
fn parse_fact_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start_matches(|c: char| c == '-' || c.is_whitespace());
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let key = rest[..close].trim();
    let content = rest[close + 1..].trim();
    if key.is_empty() || content.is_empty() {
        return None;
    }
    Some((key, content))
}

/// Maximum characters for the checkpoint conversation summary stored to memory.
const CHECKPOINT_MAX_SUMMARY_CHARS: usize = 3_000;

/// Persist a snapshot of the current conversation to brain.db so it survives
/// pod restarts and `/new` resets.
///
/// Two things are stored:
/// 1. **Durable facts** — extracted by `flush_durable_facts()` into `Core` memory.
/// 2. **Conversation summary** — an LLM-generated bullet-point summary stored
///    under a timestamped key in the `Conversation` category.
///
/// Returns a human-readable status message.
pub(crate) async fn checkpoint_conversation(
    history: &[ChatMessage],
    provider: &dyn Provider,
    model: &str,
    memory: &dyn Memory,
) -> String {
    let non_system: Vec<_> = history.iter().filter(|m| m.role != "system").collect();
    if non_system.is_empty() {
        return "Nothing to checkpoint — conversation is empty.".to_string();
    }

    let transcript =
        build_compaction_transcript(&non_system.into_iter().cloned().collect::<Vec<_>>());

    // Step 1: Extract and persist durable facts.
    let fact_count_before = memory.count().await.unwrap_or(0);
    flush_durable_facts(provider, model, &transcript, memory).await;
    let fact_count_after = memory.count().await.unwrap_or(0);
    let facts_stored = fact_count_after.saturating_sub(fact_count_before);

    // Step 2: Generate a conversation summary.
    let summarizer_system = "You are a conversation checkpoint engine. \
        Summarize the entire conversation into concise context that would help \
        the same agent resume seamlessly after a restart. \
        Preserve: user preferences, commitments, decisions, unresolved tasks, \
        key facts, what was being worked on, and any pending action items. \
        Omit: filler, greetings, verbose tool logs. \
        Output plain text bullet points only.";

    let summarizer_user = format!(
        "Summarize this conversation for checkpoint preservation \
         (max 20 bullet points).\n\n{}",
        transcript
    );

    let summary = match provider
        .chat_with_system(Some(summarizer_system), &summarizer_user, model, 0.2)
        .await
    {
        Ok(raw) => truncate_with_ellipsis(&raw, CHECKPOINT_MAX_SUMMARY_CHARS),
        Err(e) => {
            tracing::warn!("Checkpoint summary generation failed: {e}");
            // Fall back to a truncated transcript so we still save something.
            truncate_with_ellipsis(&transcript, CHECKPOINT_MAX_SUMMARY_CHARS)
        }
    };

    // Step 3: Store the summary with a timestamped key.
    let now = chrono::Local::now();
    let key = format!("checkpoint_{}", now.format("%Y%m%d_%H%M%S"));
    let content = format!(
        "[Conversation checkpoint — {}]\n{}",
        now.format("%Y-%m-%d %H:%M:%S"),
        summary.trim()
    );

    match memory
        .store(&key, &content, MemoryCategory::Core, None)
        .await
    {
        Ok(()) => {
            tracing::info!(
                "Checkpoint saved: {key} ({} chars, {facts_stored} new facts)",
                content.len()
            );
            let mut msg = format!(
                "Checkpoint saved ({} conversation turns summarized).",
                history.iter().filter(|m| m.role != "system").count()
            );
            if facts_stored > 0 {
                msg.push_str(&format!(" Extracted {facts_stored} durable fact(s)."));
            }
            msg
        }
        Err(e) => {
            tracing::warn!("Failed to store checkpoint: {e}");
            format!("Checkpoint failed: {e}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ChatRequest, ChatResponse, Provider};
    use async_trait::async_trait;

    struct StaticSummaryProvider;

    #[async_trait]
    impl Provider for StaticSummaryProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("- summarized context".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("- summarized context".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
                quota_metadata: None,
                stop_reason: None,
                raw_stop_reason: None,
            })
        }
    }

    fn assistant_with_tool_call(id: &str) -> ChatMessage {
        ChatMessage::assistant(format!(
            "{{\"content\":\"\",\"tool_calls\":[{{\"id\":\"{id}\",\"name\":\"shell\",\"arguments\":\"{{}}\"}}]}}"
        ))
    }

    fn tool_result(id: &str) -> ChatMessage {
        ChatMessage::tool(format!("{{\"tool_call_id\":\"{id}\",\"content\":\"ok\"}}"))
    }

    #[test]
    fn trim_history_avoids_orphan_tool_at_boundary() {
        let mut history = vec![
            ChatMessage::user("old"),
            assistant_with_tool_call("call_1"),
            tool_result("call_1"),
            ChatMessage::user("recent"),
        ];

        trim_history(&mut history, 2);

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[0].content, "recent");
    }

    #[tokio::test]
    async fn auto_compact_history_does_not_split_tool_run_boundary() {
        let mut history = vec![
            ChatMessage::user("oldest"),
            assistant_with_tool_call("call_2"),
            tool_result("call_2"),
        ];
        for idx in 0..19 {
            history.push(ChatMessage::user(format!("recent-{idx}")));
        }
        // 22 non-system messages => compaction with max_history=21 would
        // previously cut right before the tool result (index 2).
        assert_eq!(history.len(), 22);

        let compacted = auto_compact_history(
            &mut history,
            &StaticSummaryProvider,
            "test-model",
            21,
            None,
            None,
        )
        .await
        .expect("compaction should succeed");

        assert!(compacted);
        assert_eq!(history[0].role, "assistant");
        assert!(
            history[0].content.contains("[Compaction summary]"),
            "summary message should replace compacted range"
        );
        assert_ne!(
            history[1].role, "tool",
            "first retained message must not be an orphan tool result"
        );
    }

    #[test]
    fn parse_fact_line_extracts_key_and_content() {
        assert_eq!(
            parse_fact_line("[preferred_language] User prefers Rust over Go"),
            Some(("preferred_language", "User prefers Rust over Go"))
        );
    }

    #[test]
    fn parse_fact_line_handles_leading_dash() {
        assert_eq!(
            parse_fact_line("- [db_choice] Project uses PostgreSQL 16"),
            Some(("db_choice", "Project uses PostgreSQL 16"))
        );
    }

    #[test]
    fn parse_fact_line_rejects_empty_key_or_content() {
        assert_eq!(parse_fact_line("[] some content"), None);
        assert_eq!(parse_fact_line("[key]"), None);
        assert_eq!(parse_fact_line("[key]  "), None);
    }

    #[test]
    fn parse_fact_line_rejects_malformed_input() {
        assert_eq!(parse_fact_line("no brackets here"), None);
        assert_eq!(parse_fact_line(""), None);
        assert_eq!(parse_fact_line("[unclosed bracket"), None);
    }

    #[tokio::test]
    async fn auto_compact_with_memory_stores_durable_facts() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        struct FactCapture {
            stored: Mutex<Vec<(String, String)>>,
        }

        #[async_trait]
        impl Memory for FactCapture {
            async fn store(
                &self,
                key: &str,
                content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                self.stored
                    .lock()
                    .unwrap()
                    .push((key.to_string(), content.to_string()));
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "fact-capture"
            }
        }

        /// Provider that returns facts for the first call (flush) and summary for the second (compaction).
        struct FlushThenSummaryProvider {
            call_count: Mutex<usize>,
        }

        #[async_trait]
        impl Provider for FlushThenSummaryProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<String> {
                let mut count = self.call_count.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    // flush_durable_facts call
                    Ok("[lang] User prefers Rust\n[db] PostgreSQL 16".to_string())
                } else {
                    // summarizer call
                    Ok("- summarized context".to_string())
                }
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some("- summarized context".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let mem = Arc::new(FactCapture {
            stored: Mutex::new(Vec::new()),
        });
        let provider = FlushThenSummaryProvider {
            call_count: Mutex::new(0),
        };

        let mut history: Vec<ChatMessage> = Vec::new();
        for i in 0..25 {
            history.push(ChatMessage::user(format!("msg-{i}")));
        }

        let compacted = auto_compact_history(
            &mut history,
            &provider,
            "test-model",
            21,
            None,
            Some(mem.as_ref()),
        )
        .await
        .expect("compaction should succeed");

        assert!(compacted);

        let stored = mem.stored.lock().unwrap();
        assert_eq!(stored.len(), 2, "should store 2 durable facts");
        assert_eq!(stored[0].0, "compaction_fact_lang");
        assert_eq!(stored[0].1, "User prefers Rust");
        assert_eq!(stored[1].0, "compaction_fact_db");
        assert_eq!(stored[1].1, "PostgreSQL 16");
    }

    #[tokio::test]
    async fn auto_compact_with_memory_caps_fact_flush_at_eight_entries() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        struct FactCapture {
            stored: Mutex<Vec<(String, String)>>,
        }

        #[async_trait]
        impl Memory for FactCapture {
            async fn store(
                &self,
                key: &str,
                content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                self.stored
                    .lock()
                    .expect("fact capture lock")
                    .push((key.to_string(), content.to_string()));
                Ok(())
            }

            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }

            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }

            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }

            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }

            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }

            async fn health_check(&self) -> bool {
                true
            }

            fn name(&self) -> &str {
                "fact-capture-cap"
            }
        }

        struct FlushManyFactsProvider {
            call_count: Mutex<usize>,
        }

        #[async_trait]
        impl Provider for FlushManyFactsProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<String> {
                let mut count = self.call_count.lock().expect("provider lock");
                *count += 1;
                if *count == 1 {
                    let lines = (0..12)
                        .map(|idx| format!("[k{idx}] fact-{idx}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(lines)
                } else {
                    Ok("- summarized context".to_string())
                }
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some("- summarized context".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let mem = Arc::new(FactCapture {
            stored: Mutex::new(Vec::new()),
        });
        let provider = FlushManyFactsProvider {
            call_count: Mutex::new(0),
        };
        let mut history = (0..30)
            .map(|idx| ChatMessage::user(format!("msg-{idx}")))
            .collect::<Vec<_>>();

        let compacted = auto_compact_history(
            &mut history,
            &provider,
            "test-model",
            21,
            None,
            Some(mem.as_ref()),
        )
        .await
        .expect("compaction should succeed");
        assert!(compacted);

        let stored = mem.stored.lock().expect("fact capture lock");
        assert_eq!(stored.len(), COMPACTION_MAX_FLUSH_FACTS);
        assert_eq!(stored[0].0, "compaction_fact_k0");
        assert_eq!(stored[7].0, "compaction_fact_k7");
    }

    #[tokio::test]
    async fn checkpoint_stores_summary_and_facts() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::{Arc, Mutex};

        struct CheckpointCapture {
            stored: Mutex<Vec<(String, String, String)>>, // (key, content, category)
        }

        #[async_trait]
        impl Memory for CheckpointCapture {
            async fn store(
                &self,
                key: &str,
                content: &str,
                category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                let cat = format!("{category:?}");
                self.stored
                    .lock()
                    .unwrap()
                    .push((key.to_string(), content.to_string(), cat));
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                let stored = self.stored.lock().unwrap();
                Ok(stored.len())
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "checkpoint-capture"
            }
        }

        /// Provider that returns facts for the first call, summary for the second.
        struct CheckpointProvider {
            call_count: Mutex<usize>,
        }

        #[async_trait]
        impl Provider for CheckpointProvider {
            async fn chat_with_system(
                &self,
                _system: Option<&str>,
                _message: &str,
                _model: &str,
                _temp: f64,
            ) -> anyhow::Result<String> {
                let mut count = self.call_count.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    Ok("[project_status] Working on Kubernetes deployment".to_string())
                } else {
                    Ok("- User discussed K8s deployment\n- Action: update image tag".to_string())
                }
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temp: f64,
            ) -> anyhow::Result<ChatResponse> {
                Ok(ChatResponse {
                    text: Some("ok".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                    quota_metadata: None,
                    stop_reason: None,
                    raw_stop_reason: None,
                })
            }
        }

        let mem = Arc::new(CheckpointCapture {
            stored: Mutex::new(Vec::new()),
        });
        let provider = CheckpointProvider {
            call_count: Mutex::new(0),
        };

        let history = vec![
            ChatMessage::system("You are an AI assistant."),
            ChatMessage::user("Deploy the new image to staging"),
            ChatMessage::assistant("I'll update the deployment manifest."),
            ChatMessage::user("Great, also bump the version tag"),
        ];

        let result = checkpoint_conversation(&history, &provider, "test-model", mem.as_ref()).await;

        assert!(
            result.contains("Checkpoint saved"),
            "should report success, got: {result}"
        );
        assert!(
            result.contains("3 conversation turns"),
            "should count non-system turns, got: {result}"
        );

        let stored = mem.stored.lock().unwrap();
        // Should have: 1 fact + 1 summary = 2 entries
        assert!(
            stored.len() >= 2,
            "expected at least 2 stored entries, got {}",
            stored.len()
        );

        // Verify the fact was stored as Core
        let fact = stored
            .iter()
            .find(|(k, _, _)| k.starts_with("compaction_fact_"));
        assert!(fact.is_some(), "should store at least one durable fact");
        assert_eq!(fact.unwrap().2, "Core");

        // Verify the checkpoint summary was stored as Core
        let checkpoint = stored.iter().find(|(k, _, _)| k.starts_with("checkpoint_"));
        assert!(checkpoint.is_some(), "should store checkpoint summary");
        assert!(
            checkpoint.unwrap().1.contains("[Conversation checkpoint"),
            "summary should have checkpoint header"
        );
        assert_eq!(checkpoint.unwrap().2, "Core");
    }

    #[tokio::test]
    async fn checkpoint_empty_conversation_returns_early() {
        use crate::memory::{MemoryCategory, MemoryEntry};
        use std::sync::Mutex;

        struct NullMemory;

        #[async_trait]
        impl Memory for NullMemory {
            async fn store(
                &self,
                _k: &str,
                _c: &str,
                _cat: MemoryCategory,
                _s: Option<&str>,
            ) -> anyhow::Result<()> {
                Ok(())
            }
            async fn recall(
                &self,
                _q: &str,
                _l: usize,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
                Ok(None)
            }
            async fn list(
                &self,
                _c: Option<&MemoryCategory>,
                _s: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                Ok(vec![])
            }
            async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
                Ok(true)
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "null"
            }
        }

        let mem = NullMemory;
        let provider = StaticSummaryProvider;

        // Only a system message — no actual conversation
        let history = vec![ChatMessage::system("You are an AI assistant.")];
        let result = checkpoint_conversation(&history, &provider, "test-model", &mem).await;
        assert!(
            result.contains("empty"),
            "should say conversation is empty, got: {result}"
        );

        // Completely empty
        let history: Vec<ChatMessage> = vec![];
        let result = checkpoint_conversation(&history, &provider, "test-model", &mem).await;
        assert!(result.contains("empty"));
    }

    // ── truncate_tool_message ────────────────────────────────────────────────

    #[test]
    fn truncate_tool_message_no_op_when_short() {
        let content = r#"{"tool_call_id":"call_1","content":"short result"}"#;
        assert_eq!(truncate_tool_message(content, 200), content);
    }

    #[test]
    fn truncate_tool_message_no_op_when_max_chars_zero() {
        let content = r#"{"tool_call_id":"call_1","content":"short result"}"#;
        assert_eq!(truncate_tool_message(content, 0), content);
    }

    #[test]
    fn truncate_tool_message_preserves_envelope_and_truncates_content() {
        let inner = "x".repeat(500);
        let content = format!(r#"{{"tool_call_id":"call_1","content":"{inner}"}}"#);
        let result = truncate_tool_message(&content, 50);
        let parsed: serde_json::Value = serde_json::from_str(&result)
            .expect("result should be valid JSON");
        // envelope fields intact
        assert_eq!(parsed["tool_call_id"], "call_1");
        // inner content was truncated
        let truncated_inner = parsed["content"].as_str().unwrap();
        assert!(
            truncated_inner.len() < inner.len(),
            "content should be shorter after truncation"
        );
        assert!(
            truncated_inner.ends_with("..."),
            "truncated content should end with ellipsis"
        );
    }

    #[test]
    fn truncate_tool_message_plain_text_fallback() {
        // No tool_call_id key — should fall back to plain truncation
        let content = "a".repeat(200);
        let result = truncate_tool_message(&content, 10);
        assert!(result.len() < content.len());
        assert!(result.ends_with("..."));
    }

    #[test]
    fn build_compaction_transcript_caps_tool_result_per_message() {
        let large_inner = "z".repeat(10_000);
        let tool_msg = ChatMessage::tool(format!(
            r#"{{"tool_call_id":"call_big","content":"{large_inner}"}}"#
        ));
        let messages = vec![
            ChatMessage::user("do something"),
            ChatMessage::assistant("ok"),
            tool_msg,
        ];
        let transcript = build_compaction_transcript(&messages);
        // The large tool result must not dominate — transcript stays manageable
        assert!(
            transcript.chars().count() < 4_000,
            "transcript should be capped well below 10 000 chars"
        );
        // The tool_call_id should still be present in the transcript
        assert!(
            transcript.contains("call_big"),
            "tool_call_id should survive truncation in transcript"
        );
    }
}
