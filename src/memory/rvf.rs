//! RVF (RuVector Format) memory backend for ZeroClaw.
//!
//! Wraps [`memory_rvf::RvfMemoryStore`] and implements the ZeroClaw
//! [`Memory`] trait. Enabled by the `memory-rvf` Cargo feature.
//!
//! The backing store is a single `.rvf` file (plus `.rvf.idx` sidecar) placed
//! inside the ZeroClaw workspace directory.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use super::traits::{Memory, MemoryCategory, MemoryEntry};

// ── Category helpers ──────────────────────────────────────────────────────────

fn category_to_str(cat: &MemoryCategory) -> String {
    cat.to_string()
}

fn str_to_category(s: &str) -> MemoryCategory {
    match s {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

// ── Entry mapping ─────────────────────────────────────────────────────────────

fn stored_to_memory(e: memory_rvf::StoredEntry) -> MemoryEntry {
    MemoryEntry {
        id: e.entry_id,
        key: e.key,
        content: e.content,
        category: str_to_category(&e.category),
        timestamp: e.timestamp,
        session_id: e.session_id,
        score: None,
    }
}

// ── Backend struct ────────────────────────────────────────────────────────────

/// ZeroClaw memory backend backed by the RuVector Format vector store.
pub struct RvfMemory {
    inner: memory_rvf::RvfMemoryStore,
}

impl RvfMemory {
    /// Open or create the store at `<workspace_dir>/zara.rvf`.
    pub async fn new(workspace_dir: &Path) -> Result<Self> {
        let rvf_path = workspace_dir.join("zara.rvf");
        let inner = memory_rvf::RvfMemoryStore::open_or_create(rvf_path).await?;
        Ok(Self { inner })
    }

    /// Sync variant for use in ZeroClaw's synchronous `create_memory_with_builders`.
    pub fn new_sync(workspace_dir: &Path) -> Result<Self> {
        let rvf_path = workspace_dir.join("zara.rvf");
        let inner = memory_rvf::RvfMemoryStore::open_or_create_sync(rvf_path)?;
        Ok(Self { inner })
    }
}

// ── Memory trait impl ─────────────────────────────────────────────────────────

#[async_trait]
impl Memory for RvfMemory {
    fn name(&self) -> &str {
        "rvf"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let cat_str = category_to_str(&category);
        self.inner
            .store_entry(key, content, &cat_str, session_id)
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let results = self.inner.recall_entries(query, limit, session_id).await?;
        Ok(results.into_iter().map(stored_to_memory).collect())
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let entry = self.inner.get_entry(key).await?;
        Ok(entry.map(stored_to_memory))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let cat_str = category.map(category_to_str);
        let results = self
            .inner
            .list_entries(cat_str.as_deref(), session_id)
            .await?;
        Ok(results.into_iter().map(stored_to_memory).collect())
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        self.inner.forget_entry(key).await
    }

    async fn count(&self) -> Result<usize> {
        Ok(self.inner.entry_count().await)
    }

    async fn health_check(&self) -> bool {
        self.inner.is_healthy().await
    }
}
