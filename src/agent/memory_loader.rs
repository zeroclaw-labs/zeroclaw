use crate::memory::{self, decay, Memory, MemoryCategory};
use async_trait::async_trait;
use std::fmt::Write;

/// Default half-life (days) for time decay in memory loading.
const LOADER_DECAY_HALF_LIFE_DAYS: f64 = 7.0;

/// Score boost applied to `Core` category memories so durable facts and
/// preferences surface even when keyword/semantic similarity is moderate.
const CORE_CATEGORY_SCORE_BOOST: f64 = 0.3;

/// Over-fetch factor: retrieve more candidates than the output limit so
/// that Core boost and re-ranking can select the best subset.
const RECALL_OVER_FETCH_FACTOR: usize = 2;

#[async_trait]
pub trait MemoryLoader: Send + Sync {
    async fn load_context(&self, memory: &dyn Memory, user_message: &str)
        -> anyhow::Result<String>;
}

pub struct DefaultMemoryLoader {
    limit: usize,
    min_relevance_score: f64,
}

impl Default for DefaultMemoryLoader {
    fn default() -> Self {
        Self {
            limit: 5,
            min_relevance_score: 0.4,
        }
    }
}

impl DefaultMemoryLoader {
    pub fn new(limit: usize, min_relevance_score: f64) -> Self {
        Self {
            limit: limit.max(1),
            min_relevance_score,
        }
    }
}

#[async_trait]
impl MemoryLoader for DefaultMemoryLoader {
    async fn load_context(
        &self,
        memory: &dyn Memory,
        user_message: &str,
    ) -> anyhow::Result<String> {
        // Over-fetch so Core-boosted entries can compete fairly after re-ranking.
        let fetch_limit = self.limit * RECALL_OVER_FETCH_FACTOR;
        let mut entries = memory.recall(user_message, fetch_limit, None).await?;
        if entries.is_empty() {
            return Ok(String::new());
        }

        // Apply time decay: older non-Core memories score lower.
        decay::apply_time_decay(&mut entries, LOADER_DECAY_HALF_LIFE_DAYS);

        // Apply Core category boost and filter by minimum relevance.
        let mut scored: Vec<_> = entries
            .iter()
            .filter(|e| !memory::is_assistant_autosave_key(&e.key))
            .filter_map(|e| {
                let base = e.score.unwrap_or(self.min_relevance_score);
                let boosted = if e.category == MemoryCategory::Core {
                    (base + CORE_CATEGORY_SCORE_BOOST).min(1.0)
                } else {
                    base
                };
                if boosted >= self.min_relevance_score {
                    Some((e, boosted))
                } else {
                    None
                }
            })
            .collect();

        // Sort by boosted score descending, then truncate to output limit.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(self.limit);

        if scored.is_empty() {
            return Ok(String::new());
        }

        let mut context = String::from("[Memory context]\n");
        for (entry, _) in &scored {
            let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
        }
        context.push('\n');
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, MemoryEntry};
    use std::sync::Arc;

    struct MockMemory;
    struct MockMemoryWithEntries {
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
            limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            if limit == 0 {
                return Ok(vec![]);
            }
            Ok(vec![MemoryEntry {
                id: "1".into(),
                key: "k".into(),
                content: "v".into(),
                category: MemoryCategory::Conversation,
                timestamp: "now".into(),
                session_id: None,
                score: None,
            }])
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
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        fn name(&self) -> &str {
            "mock"
        }
    }

    #[async_trait]
    impl Memory for MockMemoryWithEntries {
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
            "mock-with-entries"
        }
    }

    #[tokio::test]
    async fn default_loader_formats_context() {
        let loader = DefaultMemoryLoader::default();
        let context = loader.load_context(&MockMemory, "hello").await.unwrap();
        assert!(context.contains("[Memory context]"));
        assert!(context.contains("- k: v"));
    }

    #[tokio::test]
    async fn default_loader_skips_legacy_assistant_autosave_entries() {
        let loader = DefaultMemoryLoader::new(5, 0.0);
        let memory = MockMemoryWithEntries {
            entries: Arc::new(vec![
                MemoryEntry {
                    id: "1".into(),
                    key: "assistant_resp_legacy".into(),
                    content: "fabricated detail".into(),
                    category: MemoryCategory::Daily,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.95),
                },
                MemoryEntry {
                    id: "2".into(),
                    key: "user_fact".into(),
                    content: "User prefers concise answers".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.9),
                },
            ]),
        };

        let context = loader.load_context(&memory, "answer style").await.unwrap();
        assert!(context.contains("user_fact"));
        assert!(!context.contains("assistant_resp_legacy"));
        assert!(!context.contains("fabricated detail"));
    }

    #[tokio::test]
    async fn core_category_boost_promotes_low_score_core_entry() {
        let loader = DefaultMemoryLoader::new(2, 0.4);
        let memory = MockMemoryWithEntries {
            entries: Arc::new(vec![
                MemoryEntry {
                    id: "1".into(),
                    key: "chat_detail".into(),
                    content: "talked about weather".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.6),
                },
                MemoryEntry {
                    id: "2".into(),
                    key: "project_rule".into(),
                    content: "always use async/await".into(),
                    category: MemoryCategory::Core,
                    timestamp: "now".into(),
                    session_id: None,
                    // Below threshold without boost (0.25 < 0.4),
                    // but above with +0.3 boost (0.55 >= 0.4).
                    score: Some(0.25),
                },
                MemoryEntry {
                    id: "3".into(),
                    key: "low_conv".into(),
                    content: "irrelevant chatter".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.2),
                },
            ]),
        };

        let context = loader.load_context(&memory, "code style").await.unwrap();
        // Core entry should survive thanks to boost
        assert!(
            context.contains("project_rule"),
            "Core entry should be promoted by boost: {context}"
        );
        // Low-score Conversation entry should be filtered out
        assert!(
            !context.contains("low_conv"),
            "Low-score non-Core entry should be filtered: {context}"
        );
    }

    #[tokio::test]
    async fn core_boost_reranks_above_conversation() {
        let loader = DefaultMemoryLoader::new(1, 0.0);
        let memory = MockMemoryWithEntries {
            entries: Arc::new(vec![
                MemoryEntry {
                    id: "1".into(),
                    key: "conv_high".into(),
                    content: "recent conversation".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.6),
                },
                MemoryEntry {
                    id: "2".into(),
                    key: "core_pref".into(),
                    content: "user prefers Rust".into(),
                    category: MemoryCategory::Core,
                    timestamp: "now".into(),
                    session_id: None,
                    // 0.5 + 0.3 boost = 0.8 > 0.6
                    score: Some(0.5),
                },
            ]),
        };

        let context = loader.load_context(&memory, "language").await.unwrap();
        // With limit=1 and Core boost, Core entry (0.8) should win over Conversation (0.6)
        assert!(
            context.contains("core_pref"),
            "Boosted Core should rank above Conversation: {context}"
        );
        assert!(
            !context.contains("conv_high"),
            "Conversation should be truncated when limit=1: {context}"
        );
    }
}
