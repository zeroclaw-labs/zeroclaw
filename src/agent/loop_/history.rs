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

        let compacted =
            auto_compact_history(&mut history, &StaticSummaryProvider, "test-model", 21, None)
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
}
