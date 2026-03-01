use crate::memory::{self, decay, Memory, MemoryCategory};
use std::fmt::Write;

/// Default half-life (days) for time decay in context building.
const CONTEXT_DECAY_HALF_LIFE_DAYS: f64 = 7.0;

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
/// Core memories are exempt from time decay (evergreen).
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
    if let Ok(mut entries) = mem.recall(user_msg, fetch_limit, session_id).await {
        // Apply time decay: older non-Core memories score lower.
        decay::apply_time_decay(&mut entries, CONTEXT_DECAY_HALF_LIFE_DAYS);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, MemoryEntry};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockMemory {
        entries: Arc<Vec<MemoryEntry>>,
    }

    #[async_trait]
    impl Memory for MockMemory {
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(self.entries.as_ref().clone())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(vec![])
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.len())
        }

        async fn health_check(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "mock-memory"
        }
    }

    #[tokio::test]
    async fn build_context_promotes_core_entries_with_score_boost() {
        let memory = MockMemory {
            entries: Arc::new(vec![
                MemoryEntry {
                    id: "1".into(),
                    key: "conv_note".into(),
                    content: "small talk".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.6),
                },
                MemoryEntry {
                    id: "2".into(),
                    key: "core_rule".into(),
                    content: "always provide tests".into(),
                    category: MemoryCategory::Core,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.2),
                },
                MemoryEntry {
                    id: "3".into(),
                    key: "conv_low".into(),
                    content: "irrelevant".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.1),
                },
            ]),
        };

        let context = build_context(&memory, "test query", 0.4, None).await;
        assert!(
            context.contains("core_rule"),
            "expected core boost to include core_rule"
        );
        assert!(
            !context.contains("conv_low"),
            "low-score non-core should be filtered"
        );
    }

    #[tokio::test]
    async fn build_context_keeps_output_limit_at_five_entries() {
        let entries = (0..8)
            .map(|idx| MemoryEntry {
                id: idx.to_string(),
                key: format!("k{idx}"),
                content: format!("v{idx}"),
                category: MemoryCategory::Conversation,
                timestamp: "now".into(),
                session_id: None,
                score: Some(0.9 - (idx as f64 * 0.01)),
            })
            .collect::<Vec<_>>();
        let memory = MockMemory {
            entries: Arc::new(entries),
        };

        let context = build_context(&memory, "limit", 0.0, None).await;
        let listed = context
            .lines()
            .filter(|line| line.starts_with("- "))
            .count();
        assert_eq!(listed, 5, "context output limit should remain 5 entries");
    }
}
