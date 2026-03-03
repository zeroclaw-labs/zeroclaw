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

pub(super) fn build_compaction_transcript(messages: &[ChatMessage]) -> String {
    let mut transcript = String::new();
    for msg in messages {
        let role = msg.role.to_uppercase();
        let _ = writeln!(transcript, "{role}: {}", msg.content.trim());
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
/// `Core` memories. Called before compaction discards old messages.
///
/// Best-effort: failures are logged but never block compaction.
async fn flush_durable_facts(
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
}
