use super::qdrant::QdrantMemory;
use super::sqlite::SqliteMemory;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use async_trait::async_trait;

/// Hybrid memory backend: SQLite (authoritative) + Qdrant (vector sync).
///
/// SQLite is the source of truth for all memory entries. Qdrant is kept
/// in sync on a best-effort basis to provide semantic vector search.
/// If Qdrant is unavailable during a store, the entry is persisted safely
/// to SQLite and a warning is logged. Running `reindex` recovers Qdrant
/// consistency by re-syncing all SQLite entries.
pub struct SqliteQdrantHybridMemory {
    sqlite: SqliteMemory,
    qdrant: QdrantMemory,
}

impl SqliteQdrantHybridMemory {
    pub fn new(sqlite: SqliteMemory, qdrant: QdrantMemory) -> Self {
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
    ) -> anyhow::Result<()> {
        // SQLite is authoritative — always store there first.
        self.sqlite
            .store(key, content, category.clone(), session_id)
            .await?;

        // Best-effort sync to Qdrant; warn on failure.
        if let Err(e) = self.qdrant.store(key, content, category, session_id).await {
            tracing::warn!(
                key,
                error = %e,
                "Qdrant sync failed during store; SQLite remains authoritative"
            );
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Try Qdrant first for semantic search.
        match self.qdrant.recall(query, limit, session_id).await {
            Ok(results) if !results.is_empty() => Ok(results),
            Ok(_) | Err(_) => {
                // Fallback to SQLite keyword/hybrid search.
                self.sqlite.recall(query, limit, session_id).await
            }
        }
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        // SQLite is authoritative for exact key lookups.
        self.sqlite.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.sqlite.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let deleted = self.sqlite.forget(key).await?;

        // Best-effort delete from Qdrant.
        if let Err(e) = self.qdrant.forget(key).await {
            tracing::warn!(
                key,
                error = %e,
                "Qdrant sync failed during forget; SQLite remains authoritative"
            );
        }

        Ok(deleted)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        // SQLite is authoritative for entry count.
        self.sqlite.count().await
    }

    async fn health_check(&self) -> bool {
        // Healthy if SQLite is healthy; Qdrant unavailability is degraded
        // but not fatal.
        self.sqlite.health_check().await
    }

    /// Two-phase reindex:
    ///
    /// **Phase 1** — Rebuild SQLite FTS5 and local embeddings by delegating
    /// to the SQLite backend reindex.
    ///
    /// **Phase 2** — Re-sync every SQLite entry to Qdrant so the vector
    /// index matches the authoritative store. Individual Qdrant failures
    /// are logged as warnings (best-effort pattern).
    async fn reindex(&self) -> anyhow::Result<usize> {
        // ── Phase 1: SQLite reindex ──────────────────────────────────
        tracing::info!("hybrid reindex phase 1: rebuilding SQLite FTS5 and embeddings");
        let sqlite_count = self.sqlite.reindex().await?;
        tracing::info!(
            "hybrid reindex phase 1 complete: {sqlite_count} embeddings rebuilt in SQLite"
        );

        // ── Phase 2: Re-sync all entries to Qdrant ──────────────────
        tracing::info!("hybrid reindex phase 2: re-syncing all entries to Qdrant");
        let entries = self.sqlite.list(None, None).await?;
        let total = entries.len();
        let mut synced: usize = 0;
        let mut failed: usize = 0;

        for entry in &entries {
            match self
                .qdrant
                .store(
                    &entry.key,
                    &entry.content,
                    entry.category.clone(),
                    entry.session_id.as_deref(),
                )
                .await
            {
                Ok(()) => synced += 1,
                Err(e) => {
                    failed += 1;
                    tracing::warn!(
                        key = %entry.key,
                        error = %e,
                        "Qdrant re-sync failed for entry; skipping"
                    );
                }
            }
        }

        tracing::info!(
            "hybrid reindex phase 2 complete: {synced}/{total} entries synced to Qdrant \
             ({failed} failed)"
        );

        Ok(synced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embeddings::{EmbeddingProvider, NoopEmbedding};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Verifies the hybrid backend name is correct.
    #[test]
    fn hybrid_name() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding);
        let qdrant =
            QdrantMemory::new_lazy("http://localhost:6333", "test_collection", None, embedder);
        let hybrid = SqliteQdrantHybridMemory::new(sqlite, qdrant);
        assert_eq!(hybrid.name(), "sqlite_qdrant_hybrid");
    }

    /// Phase 1 (SQLite reindex) succeeds even when Qdrant is unreachable.
    /// The test exercises reindex on the hybrid backend against an
    /// intentionally unreachable Qdrant endpoint.
    #[tokio::test]
    async fn reindex_succeeds_with_unreachable_qdrant() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding);
        // Point at a non-existent Qdrant instance.
        let qdrant =
            QdrantMemory::new_lazy("http://127.0.0.1:1", "test_collection", None, embedder);
        let hybrid = SqliteQdrantHybridMemory::new(sqlite, qdrant);

        // Store entries directly via the SQLite backend so they exist
        // for reindex without requiring Qdrant.
        hybrid
            .sqlite
            .store("key1", "value one", MemoryCategory::Core, None)
            .await
            .unwrap();
        hybrid
            .sqlite
            .store("key2", "value two", MemoryCategory::Daily, None)
            .await
            .unwrap();

        // Reindex should succeed — phase 1 rebuilds SQLite, phase 2
        // logs warnings for unreachable Qdrant but does not error.
        let result = hybrid.reindex().await;
        assert!(result.is_ok(), "reindex must not fail when Qdrant is down");

        // SQLite data should still be intact.
        let count = hybrid.count().await.unwrap();
        assert_eq!(count, 2);
    }

    /// Store via hybrid falls back gracefully when Qdrant is unreachable.
    #[tokio::test]
    async fn store_succeeds_with_unreachable_qdrant() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding);
        let qdrant =
            QdrantMemory::new_lazy("http://127.0.0.1:1", "test_collection", None, embedder);
        let hybrid = SqliteQdrantHybridMemory::new(sqlite, qdrant);

        hybrid
            .store("lang", "Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let entry = hybrid.get("lang").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "Rust");
    }

    /// Recall falls back to SQLite when Qdrant is unreachable.
    #[tokio::test]
    async fn recall_falls_back_to_sqlite() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding);
        let qdrant =
            QdrantMemory::new_lazy("http://127.0.0.1:1", "test_collection", None, embedder);
        let hybrid = SqliteQdrantHybridMemory::new(sqlite, qdrant);

        hybrid
            .store("note", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = hybrid.recall("fast", 10, None).await.unwrap();
        assert!(
            results.iter().any(|e| e.content.contains("Rust is fast")),
            "SQLite fallback recall should find the entry"
        );
    }

    /// Forget succeeds even when Qdrant is unreachable.
    #[tokio::test]
    async fn forget_succeeds_with_unreachable_qdrant() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding);
        let qdrant =
            QdrantMemory::new_lazy("http://127.0.0.1:1", "test_collection", None, embedder);
        let hybrid = SqliteQdrantHybridMemory::new(sqlite, qdrant);

        hybrid
            .store("temp", "temporary data", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let deleted = hybrid.forget("temp").await.unwrap();
        assert!(deleted);

        let entry = hybrid.get("temp").await.unwrap();
        assert!(entry.is_none());
    }

    /// Reindex on empty DB returns zero.
    #[tokio::test]
    async fn reindex_empty_db() {
        let tmp = TempDir::new().unwrap();
        let sqlite = SqliteMemory::new(tmp.path()).unwrap();
        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(NoopEmbedding);
        let qdrant =
            QdrantMemory::new_lazy("http://127.0.0.1:1", "test_collection", None, embedder);
        let hybrid = SqliteQdrantHybridMemory::new(sqlite, qdrant);

        let count = hybrid.reindex().await.unwrap();
        assert_eq!(count, 0);
    }
}
