mod sync_state;
mod tls;

use super::postgres::PostgresMemory;
use super::qdrant::QdrantMemory;
use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::Result;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Arc;
use sync_state::{SyncOp, SyncStateStore};

pub struct PostgresQdrantHybridMemory {
    postgres: Arc<PostgresMemory>,
    qdrant: Arc<QdrantMemory>,
    sync_state: Arc<SyncStateStore>,
}

impl PostgresQdrantHybridMemory {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db_url: &str,
        schema: &str,
        table: &str,
        connect_timeout_secs: Option<u64>,
        tls_mode: bool,
        qdrant: QdrantMemory,
    ) -> Result<Self> {
        let postgres = PostgresMemory::new(db_url, schema, table, connect_timeout_secs, tls_mode)?;
        let sync_state = SyncStateStore::new(db_url, schema, connect_timeout_secs, tls_mode)?;
        Ok(Self {
            postgres: Arc::new(postgres),
            qdrant: Arc::new(qdrant),
            sync_state: Arc::new(sync_state),
        })
    }

    fn content_hash(
        key: &str,
        content: &str,
        category: &MemoryCategory,
        session_id: Option<&str>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(content.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(category.to_string().as_bytes());
        hasher.update(b"\x1f");
        hasher.update(session_id.unwrap_or_default().as_bytes());
        hex::encode(hasher.finalize())
    }

    async fn sync_after_store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let hash = Self::content_hash(key, content, &category, session_id);
        if let Err(err) = self
            .sync_state
            .set_pending(key, SyncOp::Upsert, Some(&hash))
            .await
        {
            tracing::warn!(key, error = %err, "Failed to mark sync state pending for upsert");
        }

        match self.qdrant.store(key, content, category, session_id).await {
            Ok(_) => {
                if let Err(err) = self.sync_state.mark_synced(key).await {
                    tracing::warn!(key, error = %err, "Failed to mark sync state synced");
                }
                Ok(())
            }
            Err(err) => {
                tracing::warn!(key, error = %err, "Hybrid Qdrant upsert failed");
                if let Err(sync_err) = self.sync_state.mark_failed(key, &err.to_string()).await {
                    tracing::warn!(key, error = %sync_err, "Failed to mark sync state failed");
                }
                Err(err)
            }
        }
    }

    async fn sync_after_delete(&self, key: &str) {
        if let Err(err) = self.sync_state.set_pending(key, SyncOp::Delete, None).await {
            tracing::warn!(key, error = %err, "Failed to mark sync state pending for delete");
        }

        match self.qdrant.forget(key).await {
            Ok(_) => {
                if let Err(err) = self.sync_state.mark_synced(key).await {
                    tracing::warn!(key, error = %err, "Failed to mark sync state synced after delete");
                }
            }
            Err(err) => {
                tracing::warn!(key, error = %err, "Hybrid Qdrant delete failed");
                if let Err(sync_err) = self.sync_state.mark_failed(key, &err.to_string()).await {
                    tracing::warn!(key, error = %sync_err, "Failed to mark sync state failed after delete");
                }
            }
        }
    }
}

#[async_trait]
impl Memory for PostgresQdrantHybridMemory {
    fn name(&self) -> &str {
        "postgres_qdrant_hybrid"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.postgres
            .store(key, content, category.clone(), session_id)
            .await?;
        if let Err(err) = self
            .sync_after_store(key, content, category, session_id)
            .await
        {
            tracing::warn!(key, error = %err, "Hybrid sync after store failed");
        }
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.postgres.recall(query, limit, session_id).await;
        }

        let candidates = match self
            .qdrant
            .recall(trimmed, limit.max(1).saturating_mul(3), session_id)
            .await
        {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(query = trimmed, error = %err, "Hybrid semantic recall failed; using Postgres fallback");
                return self.postgres.recall(trimmed, limit, session_id).await;
            }
        };

        if candidates.is_empty() {
            return self.postgres.recall(trimmed, limit, session_id).await;
        }

        let mut seen = HashSet::new();
        let mut merged = Vec::with_capacity(limit);
        for candidate in candidates {
            if !seen.insert(candidate.key.clone()) {
                continue;
            }
            if let Some(mut entry) = self.postgres.get(&candidate.key).await? {
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
        }

        if merged.is_empty() {
            return self.postgres.recall(trimmed, limit, session_id).await;
        }

        Ok(merged)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        self.postgres.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        self.postgres.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let deleted = self.postgres.forget(key).await?;
        if deleted {
            self.sync_after_delete(key).await;
        }
        Ok(deleted)
    }

    async fn count(&self) -> Result<usize> {
        self.postgres.count().await
    }

    async fn health_check(&self) -> bool {
        let postgres_ok = self.postgres.health_check().await;
        if !postgres_ok {
            return false;
        }
        if !self.qdrant.health_check().await {
            tracing::warn!("Hybrid memory degraded: Qdrant health check failed");
        }
        true
    }

    async fn reindex(
        &self,
        progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
    ) -> Result<usize> {
        let entries = self.postgres.list(None, None).await?;
        let total = entries.len();
        let mut synced = 0usize;

        for (idx, entry) in entries.into_iter().enumerate() {
            if let Err(err) = self
                .sync_after_store(
                    &entry.key,
                    &entry.content,
                    entry.category.clone(),
                    entry.session_id.as_deref(),
                )
                .await
            {
                tracing::warn!(key = %entry.key, error = %err, "Hybrid reindex sync failed");
            } else {
                synced += 1;
            }
            if let Some(cb) = progress_callback.as_ref() {
                cb(idx + 1, total);
            }
        }

        Ok(synced)
    }
}
