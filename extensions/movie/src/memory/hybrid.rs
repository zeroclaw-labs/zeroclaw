use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

/// Composite memory backend:
/// - SQLite remains authoritative for metadata/content/filtering.
/// - Qdrant provides semantic ranking candidates.
pub struct SqliteQdrantHybridMemory {
    sqlite: Arc<dyn Memory>,
    qdrant: Arc<dyn Memory>,
}

impl SqliteQdrantHybridMemory {
    pub fn new(sqlite: Arc<dyn Memory>, qdrant: Arc<dyn Memory>) -> Self {
        Self { sqlite, qdrant }
    }
}

#[async_trait]
impl Memory for SqliteQdrantHybridMemory {
    fn name(&self) -> &str {
        "sqlite_qdrant_hybrid"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        // SQLite is authoritative. Fail only if local persistence fails.
        self.sqlite
            .store(key, content, category.clone(), session_id)
            .await?;

        // Best-effort vector sync to Qdrant.
        if let Err(err) = self.qdrant.store(key, content, category, session_id).await {
            tracing::warn!(
                key,
                error = %err,
                "Hybrid memory vector sync failed; SQLite entry was stored"
            );
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let trimmed_query = query.trim();
        if trimmed_query.is_empty() {
            return self.sqlite.recall(query, limit, session_id).await;
        }

        let qdrant_candidates = match self
            .qdrant
            .recall(trimmed_query, limit.max(1).saturating_mul(3), session_id)
            .await
        {
            Ok(candidates) => candidates,
            Err(err) => {
                tracing::warn!(
                    query = trimmed_query,
                    error = %err,
                    "Hybrid memory semantic recall failed; falling back to SQLite recall"
                );
                return self.sqlite.recall(trimmed_query, limit, session_id).await;
            }
        };

        if qdrant_candidates.is_empty() {
            return self.sqlite.recall(trimmed_query, limit, session_id).await;
        }

        let mut seen_keys = HashSet::new();
        let mut merged = Vec::with_capacity(limit);

        for candidate in qdrant_candidates {
            if !seen_keys.insert(candidate.key.clone()) {
                continue;
            }

            match self.sqlite.get(&candidate.key).await {
                Ok(Some(mut entry)) => {
                    if let Some(filter_sid) = session_id {
                        if entry.session_id.as_deref() != Some(filter_sid) {
                            continue;
                        }
                    }
                    entry.score = candidate.score;
                    merged.push(entry);
                    if merged.len() >= limit {
                        break;
                    }
                }
                Ok(None) => {
                    // Ignore Qdrant candidates that no longer exist in SQLite.
                }
                Err(err) => {
                    tracing::warn!(
                        key = candidate.key,
                        error = %err,
                        "Hybrid memory failed to load SQLite row for Qdrant candidate"
                    );
                }
            }
        }

        if merged.is_empty() {
            return self.sqlite.recall(trimmed_query, limit, session_id).await;
        }

        Ok(merged)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        self.sqlite.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.sqlite.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let removed = self.sqlite.forget(key).await?;
        if let Err(err) = self.qdrant.forget(key).await {
            tracing::warn!(
                key,
                error = %err,
                "Hybrid memory vector delete failed; SQLite delete result preserved"
            );
        }
        Ok(removed)
    }

    async fn count(&self) -> Result<usize> {
        self.sqlite.count().await
    }

    async fn health_check(&self) -> bool {
        let sqlite_ok = self.sqlite.health_check().await;
        if !sqlite_ok {
            return false;
        }

        if !self.qdrant.health_check().await {
            tracing::warn!("Hybrid memory Qdrant health check failed; SQLite remains available");
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Memory, MemoryCategory, MemoryEntry, SqliteMemory};
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct StubQdrantMemory {
        recall_results: Vec<MemoryEntry>,
        fail_store: bool,
        fail_recall: bool,
        forget_calls: Mutex<Vec<String>>,
    }

    impl StubQdrantMemory {
        fn new(recall_results: Vec<MemoryEntry>, fail_store: bool, fail_recall: bool) -> Self {
            Self {
                recall_results,
                fail_store,
                fail_recall,
                forget_calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for StubQdrantMemory {
        fn name(&self) -> &str {
            "qdrant_stub"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> Result<()> {
            if self.fail_store {
                anyhow::bail!("simulated qdrant store failure");
            }
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
        ) -> Result<Vec<MemoryEntry>> {
            if self.fail_recall {
                anyhow::bail!("simulated qdrant recall failure");
            }
            Ok(self.recall_results.clone())
        }

        async fn get(&self, _key: &str) -> Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, key: &str) -> Result<bool> {
            self.forget_calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(key.to_string());
            Ok(true)
        }

        async fn count(&self) -> Result<usize> {
            Ok(self.recall_results.len())
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    fn temp_sqlite() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(sqlite))
    }

    fn make_qdrant_entry(key: &str, score: f64) -> MemoryEntry {
        MemoryEntry {
            id: format!("vec-{key}"),
            key: key.to_string(),
            content: "vector payload".to_string(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-27T00:00:00Z".to_string(),
            session_id: None,
            score: Some(score),
        }
    }

    #[tokio::test]
    async fn store_keeps_sqlite_when_qdrant_sync_fails() {
        let (_tmp, sqlite) = temp_sqlite();
        let qdrant: Arc<dyn Memory> = Arc::new(StubQdrantMemory::new(Vec::new(), true, false));
        let hybrid = SqliteQdrantHybridMemory::new(Arc::clone(&sqlite), qdrant);

        hybrid
            .store("fav_lang", "Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let stored = sqlite.get("fav_lang").await.unwrap();
        assert!(stored.is_some(), "SQLite should remain authoritative");
    }

    #[tokio::test]
    async fn recall_joins_qdrant_ranking_with_sqlite_rows() {
        let (_tmp, sqlite) = temp_sqlite();
        sqlite
            .store("a", "alpha from sqlite", MemoryCategory::Core, None)
            .await
            .unwrap();
        sqlite
            .store("b", "beta from sqlite", MemoryCategory::Core, None)
            .await
            .unwrap();

        let qdrant: Arc<dyn Memory> = Arc::new(StubQdrantMemory::new(
            vec![make_qdrant_entry("b", 0.91), make_qdrant_entry("a", 0.72)],
            false,
            false,
        ));
        let hybrid = SqliteQdrantHybridMemory::new(Arc::clone(&sqlite), qdrant);

        let recalled = hybrid.recall("rank semantically", 2, None).await.unwrap();
        assert_eq!(recalled.len(), 2);
        assert_eq!(recalled[0].key, "b");
        assert_eq!(recalled[0].content, "beta from sqlite");
        assert_eq!(recalled[0].score, Some(0.91));
        assert_eq!(recalled[1].key, "a");
        assert_eq!(recalled[1].score, Some(0.72));
    }

    #[tokio::test]
    async fn recall_falls_back_to_sqlite_when_qdrant_fails() {
        let (_tmp, sqlite) = temp_sqlite();
        sqlite
            .store(
                "topic",
                "hybrid fallback should still find this",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();

        let qdrant: Arc<dyn Memory> = Arc::new(StubQdrantMemory::new(Vec::new(), false, true));
        let hybrid = SqliteQdrantHybridMemory::new(Arc::clone(&sqlite), qdrant);

        let recalled = hybrid.recall("fallback", 5, None).await.unwrap();
        assert!(
            recalled.iter().any(|entry| entry.key == "topic"),
            "SQLite fallback should provide recall results when Qdrant is unavailable"
        );
    }
}
