use async_trait::async_trait;
use std::fmt::Write;
use std::time::Instant;
use zeroclaw_memory::{self, Memory, decay};

use crate::observability::{Observer, ObserverEvent};

use super::loop_::make_query_summary;

#[async_trait]
pub trait MemoryLoader: Send + Sync {
    /// Loads a memory-context preamble for a user message.
    ///
    /// Implementations MUST emit a `ObserverEvent::MemoryRecall` event via
    /// `observer` for every recall call they perform — both on success and
    /// failure paths — so OTel/log observers can attribute per-turn memory
    /// cost. The agent runtime relies on this for end-to-end visibility
    /// of the implicit recall that runs at the start of each turn.
    async fn load_context(
        &self,
        memory: &dyn Memory,
        observer: &dyn Observer,
        user_message: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<String>;
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
        observer: &dyn Observer,
        user_message: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let backend = memory.name().to_string();
        let query_summary = make_query_summary(user_message);

        let start = Instant::now();
        let recall_result = memory
            .recall(user_message, self.limit, session_id, None, None)
            .await;
        let duration = start.elapsed();

        let mut entries = match recall_result {
            Ok(entries) => {
                observer.record_event(&ObserverEvent::MemoryRecall {
                    query_summary,
                    duration,
                    num_entries: entries.len(),
                    backend,
                    success: true,
                });
                entries
            }
            Err(e) => {
                observer.record_event(&ObserverEvent::MemoryRecall {
                    query_summary,
                    duration,
                    num_entries: 0,
                    backend,
                    success: false,
                });
                return Err(e);
            }
        };
        if entries.is_empty() {
            return Ok(String::new());
        }

        // Apply time decay: older non-Core memories score lower
        decay::apply_time_decay(&mut entries, decay::DEFAULT_HALF_LIFE_DAYS);

        let mut context = String::from("[Memory context]\n");
        for entry in entries {
            if zeroclaw_memory::is_assistant_autosave_key(&entry.key) {
                continue;
            }
            if zeroclaw_memory::is_user_autosave_key(&entry.key) {
                continue;
            }
            if zeroclaw_memory::should_skip_autosave_content(&entry.content) {
                continue;
            }
            if let Some(score) = entry.score
                && score < self.min_relevance_score
            {
                continue;
            }
            let _ = writeln!(context, "- {}: {}", entry.key, entry.content);
        }

        // If all entries were below threshold, return empty
        if context == "[Memory context]\n" {
            return Ok(String::new());
        }

        context.push_str("[/Memory context]\n\n");
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::NoopObserver;
    use std::sync::Arc;
    use zeroclaw_memory::{Memory, MemoryCategory, MemoryEntry};

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
            _since: Option<&str>,
            _until: Option<&str>,
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
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
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
            _since: Option<&str>,
            _until: Option<&str>,
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
        let context = loader
            .load_context(&MockMemory, &NoopObserver, "hello", None)
            .await
            .unwrap();
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
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                },
                MemoryEntry {
                    id: "2".into(),
                    key: "user_fact".into(),
                    content: "User prefers concise answers".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.9),
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                },
            ]),
        };

        let context = loader
            .load_context(&memory, &NoopObserver, "answer style", None)
            .await
            .unwrap();
        assert!(context.contains("user_fact"));
        assert!(!context.contains("assistant_resp_legacy"));
        assert!(!context.contains("fabricated detail"));
    }

    #[tokio::test]
    async fn default_loader_skips_user_autosave_entries() {
        let loader = DefaultMemoryLoader::new(5, 0.0);
        let memory = MockMemoryWithEntries {
            entries: Arc::new(vec![
                MemoryEntry {
                    id: "1".into(),
                    key: "user_msg_e5f6g7h8".into(),
                    content: "User message embedding prior context verbatim".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.95),
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                },
                MemoryEntry {
                    id: "2".into(),
                    key: "user_fact".into(),
                    content: "User prefers concise answers".into(),
                    category: MemoryCategory::Conversation,
                    timestamp: "now".into(),
                    session_id: None,
                    score: Some(0.9),
                    namespace: "default".into(),
                    importance: None,
                    superseded_by: None,
                },
            ]),
        };

        let context = loader
            .load_context(&memory, &NoopObserver, "answer style", None)
            .await
            .unwrap();
        assert!(context.contains("user_fact"));
        assert!(!context.contains("user_msg_e5f6g7h8"));
        assert!(!context.contains("embedding prior context"));
    }
}
