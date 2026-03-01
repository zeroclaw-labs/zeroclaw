use crate::memory::{self, Memory, MemoryCategory};
use std::fmt::Write;

/// Score boost applied to `Core` category memories so durable facts and
/// preferences surface even when keyword/semantic similarity is moderate.
const CORE_CATEGORY_SCORE_BOOST: f64 = 0.3;

/// Maximum number of memory entries included in the context preamble.
const CONTEXT_ENTRY_LIMIT: usize = 5;

/// Over-fetch factor: retrieve more candidates than the output limit so
/// that Core boost and re-ranking can select the best subset.
const RECALL_OVER_FETCH_FACTOR: usize = 2;

/// Build context preamble by searching memory for relevant entries.
/// Entries with a hybrid score below `min_relevance_score` are dropped to
/// prevent unrelated memories from bleeding into the conversation.
///
/// `Core` category memories receive a score boost so that durable facts,
/// preferences, and project rules are more likely to appear in context
/// even when semantic similarity to the current message is moderate.
pub(super) async fn build_context(
    mem: &dyn Memory,
    user_msg: &str,
    min_relevance_score: f64,
    session_id: Option<&str>,
) -> String {
    let mut context = String::new();

    // Over-fetch so Core-boosted entries can compete fairly after re-ranking.
    let fetch_limit = CONTEXT_ENTRY_LIMIT * RECALL_OVER_FETCH_FACTOR;
    if let Ok(entries) = mem.recall(user_msg, fetch_limit, session_id).await {
        // Apply Core category boost and filter by minimum relevance.
        let mut scored: Vec<_> = entries
            .iter()
            .filter(|e| !memory::is_assistant_autosave_key(&e.key))
            .filter_map(|e| {
                let base = e.score.unwrap_or(min_relevance_score);
                let boosted = if e.category == MemoryCategory::Core {
                    (base + CORE_CATEGORY_SCORE_BOOST).min(1.0)
                } else {
                    base
                };
                if boosted >= min_relevance_score {
                    Some((e, boosted))
                } else {
                    None
                }
            })
            .collect();

        // Sort by boosted score descending, then truncate to output limit.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(CONTEXT_ENTRY_LIMIT);

        if !scored.is_empty() {
            context.push_str("[Memory context]\n");
            for (entry, _) in &scored {
                let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
            }
            context.push('\n');
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
