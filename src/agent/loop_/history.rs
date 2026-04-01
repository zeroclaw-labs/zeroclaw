use crate::providers::{ChatMessage, Provider};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::fmt::Write;

/// Keep this many most-recent non-system messages after compaction.
/// 12 messages provides enough context for coherent follow-up while keeping
/// the body size well within the 2MB gateway limit (~50-80KB typical).
const COMPACTION_KEEP_RECENT_MESSAGES: usize = 12;

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

    // NOTE: Compaction uses the economy tier model (MiniMax M2.7) when available
    // via the task-based routing in billing/llm_router.rs (TaskCategory::Compaction).
    // The caller should pass the economy model here to save ~88% vs Opus 4.6.
    //
    // MiniMax M2.7 language enforcement: MUST specify output language explicitly
    // because M2.7 tends to mix Russian, Chinese, Arabic into responses.
    let summarizer_system = "You are a conversation compaction engine. Summarize older chat history into concise context for future turns. Preserve: user preferences, commitments, decisions, unresolved tasks, key facts. Omit: filler, repeated chit-chat, verbose tool logs. Output plain text bullet points only.\n\nCRITICAL LANGUAGE RULE: You MUST respond in the SAME language as the majority of the conversation. If the conversation is in Korean, respond ONLY in Korean. If in English, respond ONLY in English. NEVER mix languages. NEVER use Russian, Chinese, or Arabic unless the conversation is in that language.";

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

// ═══════════════════════════════════════════════════════════════════
// Layer 1: Per-Turn Attachment Detection & Memo Substitution
// ═══════════════════════════════════════════════════════════════════

/// Minimum size for a content block to be considered an "attachment" worth memo-ing.
/// Blocks smaller than this are kept verbatim even if they match attachment patterns.
const ATTACHMENT_MIN_CHARS: usize = 500;

/// Scan a message for attached documents, citations, search results, code blocks,
/// and tool outputs. Replace detected attachment blocks with structured YAML-like
/// memos while preserving the conversational text verbatim.
///
/// This is called **immediately** when a message is added to history[], not deferred
/// to compaction. It's pure string processing — zero LLM cost.
///
/// Returns `None` if no attachments detected (message should be kept as-is).
/// Returns `Some(compressed)` with attachment blocks replaced by memos.
pub(super) fn memo_substitute_attachments(content: &str) -> Option<String> {
    let regions = detect_attachment_regions(content);
    if regions.is_empty() {
        return None;
    }

    let mut result = String::with_capacity(content.len());
    let chars: Vec<char> = content.chars().collect();
    let mut cursor = 0;

    for region in &regions {
        // Keep text before the attachment verbatim
        if region.start > cursor {
            let before: String = chars[cursor..region.start].iter().collect();
            result.push_str(&before);
        }

        // Replace attachment with memo
        let attachment_text: String = chars[region.start..region.end].iter().collect();
        let memo = build_attachment_memo(&attachment_text, &region.kind);
        result.push_str(&memo);

        cursor = region.end;
    }

    // Keep text after the last attachment
    if cursor < chars.len() {
        let after: String = chars[cursor..].iter().collect();
        result.push_str(&after);
    }

    // Only return Some if we actually compressed something meaningful
    if result.chars().count() < content.chars().count() - 100 {
        Some(result)
    } else {
        None
    }
}

/// A detected attachment region within a message.
struct AttachmentRegion {
    start: usize, // char index
    end: usize,   // char index (exclusive)
    kind: AttachmentKind,
}

#[derive(Clone, Copy)]
enum AttachmentKind {
    CodeBlock,
    SearchResult,
    ToolResult,
    Blockquote,
    Document,
}

impl AttachmentKind {
    fn label(&self) -> &'static str {
        match self {
            Self::CodeBlock => "코드",
            Self::SearchResult => "검색결과",
            Self::ToolResult => "도구응답",
            Self::Blockquote => "인용문",
            Self::Document => "문서/첨부",
        }
    }
}

/// Detect attachment-like regions in a message using content-based heuristics.
/// Returns regions sorted by start position, non-overlapping.
fn detect_attachment_regions(content: &str) -> Vec<AttachmentRegion> {
    let chars: Vec<char> = content.chars().collect();
    let lines: Vec<&str> = content.lines().collect();
    let mut regions = Vec::new();

    // Track char offsets for each line
    let mut line_starts = Vec::with_capacity(lines.len());
    let mut char_offset = 0;
    for line in &lines {
        line_starts.push(char_offset);
        char_offset += line.chars().count() + 1; // +1 for newline
    }

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();

        // ── Code blocks (``` ... ```) ──
        if line.starts_with("```") {
            let block_start = line_starts[i];
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim().starts_with("```") {
                j += 1;
            }
            if j < lines.len() {
                j += 1; // include closing ```
            }
            let block_end = if j < lines.len() {
                line_starts[j]
            } else {
                chars.len()
            };
            let block_chars = block_end - block_start;
            if block_chars >= ATTACHMENT_MIN_CHARS {
                regions.push(AttachmentRegion {
                    start: block_start,
                    end: block_end,
                    kind: AttachmentKind::CodeBlock,
                });
            }
            i = j;
            continue;
        }

        // ── Blockquotes (consecutive > lines) ──
        if line.starts_with('>') {
            let block_start = line_starts[i];
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().starts_with('>') {
                j += 1;
            }
            let block_end = if j < lines.len() {
                line_starts[j]
            } else {
                chars.len()
            };
            let block_chars = block_end - block_start;
            if block_chars >= ATTACHMENT_MIN_CHARS {
                regions.push(AttachmentRegion {
                    start: block_start,
                    end: block_end,
                    kind: AttachmentKind::Blockquote,
                });
            }
            i = j;
            continue;
        }

        // ── Search results (3+ consecutive URL lines) ──
        if line.starts_with("http://") || line.starts_with("https://")
            || line.contains("](http") || line.contains("출처:") || line.contains("Source:")
        {
            let block_start = line_starts[i];
            let mut j = i + 1;
            let mut url_count = 1;
            while j < lines.len() {
                let l = lines[j].trim();
                if l.contains("http://") || l.contains("https://")
                    || l.contains("출처:") || l.contains("Source:")
                    || l.starts_with("- ") || l.starts_with("* ")
                {
                    url_count += 1;
                    j += 1;
                } else if l.is_empty() {
                    j += 1; // skip blank lines between results
                } else {
                    break;
                }
            }
            if url_count >= 3 {
                let block_end = if j < lines.len() {
                    line_starts[j]
                } else {
                    chars.len()
                };
                let block_chars = block_end - block_start;
                if block_chars >= ATTACHMENT_MIN_CHARS {
                    regions.push(AttachmentRegion {
                        start: block_start,
                        end: block_end,
                        kind: AttachmentKind::SearchResult,
                    });
                    i = j;
                    continue;
                }
            }
        }

        // ── Tool results (JSON blocks, table blocks) ──
        if line.starts_with('{') || line.starts_with('[') || line.starts_with("|---") {
            let block_start = line_starts[i];
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j].trim();
                if l.is_empty() && j + 1 < lines.len() {
                    // Check if content continues after blank line
                    let next = lines[j + 1].trim();
                    if next.starts_with('{') || next.starts_with('|') || next.starts_with('"') {
                        j += 1;
                    } else {
                        break;
                    }
                } else if l.starts_with('}') || l.starts_with(']') || l.starts_with('|')
                    || l.starts_with('"') || l.starts_with('{')
                {
                    j += 1;
                } else {
                    break;
                }
            }
            let block_end = if j < lines.len() {
                line_starts[j]
            } else {
                chars.len()
            };
            let block_chars = block_end - block_start;
            if block_chars >= ATTACHMENT_MIN_CHARS {
                regions.push(AttachmentRegion {
                    start: block_start,
                    end: block_end,
                    kind: AttachmentKind::ToolResult,
                });
                i = j;
                continue;
            }
        }

        // ── Document markers (explicit file references) ──
        if line.contains("[Document:") || line.contains("[PDF:") || line.contains("[IMAGE:")
            || line.contains("[HWPX:") || line.contains("[DOCX:")
        {
            let block_start = line_starts[i];
            // Scan forward to find the end of the document content
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim().is_empty() {
                j += 1;
            }
            let block_end = if j < lines.len() {
                line_starts[j]
            } else {
                chars.len()
            };
            let block_chars = block_end - block_start;
            if block_chars >= ATTACHMENT_MIN_CHARS {
                regions.push(AttachmentRegion {
                    start: block_start,
                    end: block_end,
                    kind: AttachmentKind::Document,
                });
                i = j;
                continue;
            }
        }

        i += 1;
    }

    regions
}

/// Build a YAML-like memo for a detected attachment block.
fn build_attachment_memo(text: &str, kind: &AttachmentKind) -> String {
    let char_count = text.chars().count();
    let summary_budget = (char_count / 10).clamp(50, 500);

    // Extract title: first non-empty line
    let title = text
        .lines()
        .find(|l| l.trim().len() > 3)
        .map(|l| {
            let t = l.trim().trim_start_matches('#').trim_start_matches('>').trim();
            if t.chars().count() > 80 {
                format!("{}...", t.chars().take(77).collect::<String>())
            } else {
                t.to_string()
            }
        })
        .unwrap_or_else(|| format!("{} ({}자)", kind.label(), char_count));

    // Extract keywords (unique meaningful words > 3 chars)
    let keywords: Vec<&str> = text
        .split_whitespace()
        .filter(|w| {
            let clean = w.trim_matches(|c: char| !c.is_alphanumeric());
            clean.chars().count() > 3
                && !matches!(clean, "the" | "and" | "for" | "that" | "this" | "with"
                    | "from" | "have" | "been" | "were" | "are" | "있는" | "하는"
                    | "것이" | "에서" | "으로" | "대한" | "통해")
        })
        .take(8)
        .collect();

    // Extract first ~summary_budget chars as summary
    let summary: String = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with("```") && !l.trim().starts_with(">"))
        .take(5)
        .flat_map(|l| l.chars().chain(std::iter::once(' ')))
        .take(summary_budget)
        .collect();

    format!(
        "\n---\n📋 첨부 메모 ({kind_label}, 원문 {char_count}자):\n\
         제목: {title}\n\
         키워드: {kw}\n\
         요약: {summary}\n\
         원문접근: memory_recall로 검색 가능\n\
         ---\n",
        kind_label = kind.label(),
        kw = keywords.join(", "),
        summary = summary.trim(),
    )
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

    // ── Layer 1: Attachment detection tests ──

    #[test]
    fn memo_substitute_no_attachments() {
        let msg = "안녕하세요, 오늘 날씨 어때요?";
        assert!(memo_substitute_attachments(msg).is_none());
    }

    #[test]
    fn memo_substitute_short_code_block_kept() {
        let msg = "코드입니다:\n```\nfn main() {}\n```\n끝";
        assert!(memo_substitute_attachments(msg).is_none());
    }

    #[test]
    fn memo_substitute_long_code_block_compressed() {
        let code = "x = 1\n".repeat(200);
        let msg = format!("설명합니다:\n```python\n{code}```\n이상입니다.");
        let result = memo_substitute_attachments(&msg);
        assert!(result.is_some());
        let compressed = result.unwrap();
        assert!(compressed.contains("첨부 메모"));
        assert!(compressed.contains("코드"));
        assert!(compressed.len() < msg.len());
        assert!(compressed.contains("설명합니다"));
        assert!(compressed.contains("이상입니다"));
    }

    #[test]
    fn memo_substitute_long_blockquote_compressed() {
        let lines = (0..100).map(|i| format!("> 인용문 라인 {i}")).collect::<Vec<_>>().join("\n");
        let msg = format!("다음은 인용문입니다:\n{lines}\n위 내용을 요약하면");
        let result = memo_substitute_attachments(&msg);
        assert!(result.is_some());
        let compressed = result.unwrap();
        assert!(compressed.contains("첨부 메모"));
        assert!(compressed.contains("인용문"));
    }

    #[test]
    fn memo_substitute_preserves_short_conversation() {
        let long_chat = "안녕하세요. ".repeat(300);
        assert!(memo_substitute_attachments(&long_chat).is_none());
    }

    #[test]
    fn detect_regions_empty() {
        assert!(detect_attachment_regions("hello world").is_empty());
    }

    #[test]
    fn detect_regions_code_block() {
        let content = format!("before\n```\n{}\n```\nafter", "code line\n".repeat(100));
        let regions = detect_attachment_regions(&content);
        assert_eq!(regions.len(), 1);
        assert!(matches!(regions[0].kind, AttachmentKind::CodeBlock));
    }
}
