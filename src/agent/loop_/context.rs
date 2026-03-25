use crate::memory::{self, Memory};
use crate::ontology::OntologyRepo;
use crate::providers::ChatMessage;
use std::fmt::Write;

/// Maximum number of long-term memory entries to recall per message.
const MAX_RECALL_ENTRIES: usize = 100;

/// Maximum number of ontology objects to search per message.
const MAX_ONTOLOGY_ENTRIES: usize = 100;

/// Build context preamble by searching both long-term memory and ontology
/// for relevant entries.  No byte-size cap is applied because memory entries
/// are already summarised and a high hit-count signals importance.
///
/// Entries with a hybrid score below `min_relevance_score` are dropped to
/// prevent unrelated memories from bleeding into the conversation.
pub(super) async fn build_context(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
    ontology: Option<&OntologyRepo>,
) -> String {
    // Pre-allocate a reasonable buffer for the combined context output.
    let mut context = String::with_capacity(4096);

    // ── Long-term memory recall ──────────────────────────────────
    if let Ok(entries) = mem.recall(user_msg, MAX_RECALL_ENTRIES, session_id).await {
        let relevant: Vec<_> = entries
            .iter()
            .filter(|e| match e.score {
                Some(score) => score >= min_relevance_score,
                None => true,
            })
            .collect();

        if !relevant.is_empty() {
            context.push_str("[Memory context]\n");
            for entry in &relevant {
                if memory::is_assistant_autosave_key(&entry.key) {
                    continue;
                }
                let line = format!("- {}: {}\n", entry.key, entry.content);
                context.push_str(&line);
            }
            if context == "[Memory context]\n" {
                context.clear();
            } else {
                context.push('\n');
            }
        }
    }

    // ── Ontology knowledge search ────────────────────────────────
    if let Some(repo) = ontology {
        // Use a generic owner id for CLI context; channel-based flows will
        // supply their own owner scoping at a higher layer.
        let owner = session_id.unwrap_or("cli_interactive");
        if let Ok(objects) =
            repo.search_objects(owner, None, user_msg, MAX_ONTOLOGY_ENTRIES)
        {
            if !objects.is_empty() {
                context.push_str("[Ontology context]\n");
                for obj in &objects {
                    let title = obj.title.as_deref().unwrap_or("(untitled)");
                    let props = if obj.properties.is_null() || obj.properties.as_object().is_some_and(|m| m.is_empty()) {
                        String::new()
                    } else {
                        obj.properties.to_string()
                    };
                    if props.is_empty() {
                        let _ = writeln!(context, "- {title}");
                    } else {
                        let _ = writeln!(context, "- {title}: {props}");
                    }
                }
                context.push('\n');
            }
        }
    }

    context
}

/// Build hardware datasheet context from RAG when peripherals are enabled.
/// Includes pin-alias lookup (e.g. "red_led" → 13) when query matches, plus retrieved chunks.
pub(super) fn build_hardware_context(
    rag: &crate::rag::HardwareRag,
    user_msg: &str,
    boards: &[String],
    chunk_limit: usize,
) -> String {
    if rag.is_empty() || boards.is_empty() {
        return String::new();
    }

    let mut context = String::new();

    // Pin aliases: when user says "red led", inject "red_led: 13" for matching boards
    let pin_ctx = rag.pin_alias_context(user_msg, boards);
    if !pin_ctx.is_empty() {
        context.push_str(&pin_ctx);
    }

    let chunks = rag.retrieve(user_msg, boards, chunk_limit);
    if chunks.is_empty() && pin_ctx.is_empty() {
        return String::new();
    }

    if !chunks.is_empty() {
        context.push_str("[Hardware documentation]\n");
    }
    for chunk in chunks {
        let board_tag = chunk.board.as_deref().unwrap_or("generic");
        let _ = writeln!(
            context,
            "--- {} ({}) ---\n{}\n",
            chunk.source, board_tag, chunk.content
        );
    }
    context.push('\n');
    context
}

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
/// This is UTF-8 safe — it counts Unicode scalar values, not bytes.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Build cross-session recent conversation context from stored turns.
///
/// Formats the most recent turns as `[Recent conversation history]` for injection
/// into the LLM context, providing conversational continuity across sessions.
///
/// # Parameters
/// - `turns`: recent conversation turns (oldest-first, chronological order)
/// - `skip_current`: number of trailing turns to skip (e.g. 1 to skip the
///   current user message that was just appended)
/// - `max_bytes`: maximum total bytes for the context block
/// - `turn_max_chars`: maximum characters per individual turn content
pub(super) fn build_cross_session_context(
    turns: &[ChatMessage],
    skip_current: usize,
    max_bytes: usize,
    turn_max_chars: usize,
) -> String {
    if turns.is_empty() {
        return String::new();
    }

    let take_count = turns.len().saturating_sub(skip_current);
    if take_count == 0 {
        return String::new();
    }

    const HEADER: &str = "[Recent conversation history]\n";

    // Pre-allocate: estimate ~80 bytes per turn to reduce reallocations for
    // large turn counts (up to 600).
    let estimated = HEADER.len() + take_count.min(600) * 80;
    let mut ctx = String::with_capacity(estimated.min(max_bytes + 256));
    ctx.push_str(HEADER);
    let mut total = HEADER.len();

    for turn in turns.iter().take(take_count) {
        let label = if turn.role == "user" { "User" } else { "Assistant" };
        let content = &turn.content;

        // Calculate line length without allocating a temporary String when
        // the turn content fits within the character limit.
        let char_count = content.chars().count();
        if char_count <= turn_max_chars {
            // Fast path: content fits — write directly into ctx.
            let line_len = label.len() + 2 + content.len() + 1; // "Label: content\n"
            if total + line_len > max_bytes {
                break;
            }
            total += line_len;
            ctx.push_str(label);
            ctx.push_str(": ");
            ctx.push_str(content);
            ctx.push('\n');
        } else {
            // Slow path: content needs truncation.
            let truncated = truncate_chars(content, turn_max_chars);
            let line_len = label.len() + 2 + truncated.len() + 1;
            if total + line_len > max_bytes {
                break;
            }
            total += line_len;
            ctx.push_str(label);
            ctx.push_str(": ");
            ctx.push_str(&truncated);
            ctx.push('\n');
        }
    }

    if ctx.len() == HEADER.len() {
        String::new()
    } else {
        ctx.push('\n');
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_ascii() {
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hi", 5), "hi");
    }

    #[test]
    fn truncate_chars_multibyte() {
        // Korean text: each character is 3 bytes in UTF-8
        let korean = "안녕하세요 반갑습니다";
        let result = truncate_chars(korean, 5);
        assert_eq!(result, "안녕하세요…");
        // Ensure no panic on multi-byte boundaries
        assert_eq!(truncate_chars(korean, 1), "안…");
    }

    #[test]
    fn build_cross_session_empty() {
        assert_eq!(build_cross_session_context(&[], 0, 16000, 600), "");
    }

    #[test]
    fn build_cross_session_skips_current() {
        let turns = vec![
            ChatMessage { role: "user".into(), content: "hello".into() },
            ChatMessage { role: "assistant".into(), content: "hi there".into() },
            ChatMessage { role: "user".into(), content: "current msg".into() },
        ];
        let ctx = build_cross_session_context(&turns, 1, 16000, 600);
        assert!(ctx.contains("User: hello"));
        assert!(ctx.contains("Assistant: hi there"));
        assert!(!ctx.contains("current msg"));
    }

    #[test]
    fn build_cross_session_respects_byte_limit() {
        let turns: Vec<ChatMessage> = (0..100)
            .map(|i| ChatMessage {
                role: "user".into(),
                content: format!("message number {i} with some content padding"),
            })
            .collect();
        let ctx = build_cross_session_context(&turns, 0, 500, 600);
        assert!(ctx.len() <= 500 + 100); // small overshoot from last line is ok
    }

    #[test]
    fn build_cross_session_truncates_long_turns() {
        let turns = vec![ChatMessage {
            role: "user".into(),
            content: "a".repeat(1000),
        }];
        let ctx = build_cross_session_context(&turns, 0, 16000, 10);
        assert!(ctx.contains(&format!("User: {}…", "a".repeat(10))));
    }
}
