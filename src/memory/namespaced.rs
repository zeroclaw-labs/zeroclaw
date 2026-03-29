//! Namespace isolation for memory operations.
//!
//! Provides a decorator `NamespacedMemory<M>` that wraps any `Memory` backend
//! and enforces a fixed namespace for all operations. Useful for delegate agents
//! to isolate their memory from other agents' memory spaces.
//!
//! All store operations redirect to `store_with_metadata()` with the configured
//! namespace, and all recall operations redirect to `recall_namespaced()`.

use super::traits::{Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use async_trait::async_trait;
use std::sync::Arc;

/// Decorator that wraps a `Memory` backend with namespace isolation.
///
/// When configured with a namespace, all memory operations are scoped to that
/// namespace, preventing cross-contamination between agents with different
/// memory namespaces.
pub struct NamespacedMemory {
    inner: Arc<dyn Memory>,
    namespace: String,
}

impl NamespacedMemory {
    /// Create a new NamespacedMemory wrapping an existing memory backend.
    pub fn new(inner: Arc<dyn Memory>, namespace: String) -> Self {
        Self { inner, namespace }
    }

    /// Get the namespace used by this decorator.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
}

#[async_trait]
impl Memory for NamespacedMemory {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.inner
            .store_with_metadata(
                key,
                content,
                category,
                session_id,
                Some(&self.namespace),
                None,
            )
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner
            .recall_namespaced(&self.namespace, query, limit, session_id, since, until)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let entry = self.inner.get(key).await?;
        // Return the entry only if it matches our namespace
        Ok(entry.filter(|e| e.namespace == self.namespace))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self.inner.list(category, session_id).await?;
        // Filter to only entries in our namespace
        Ok(entries
            .into_iter()
            .filter(|e| e.namespace == self.namespace)
            .collect())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        // First verify the entry is in our namespace before forgetting
        if let Some(entry) = self.inner.get(key).await? {
            if entry.namespace == self.namespace {
                return self.inner.forget(key).await;
            }
        }
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let entries = self.inner.list(None, None).await?;
        Ok(entries
            .into_iter()
            .filter(|e| e.namespace == self.namespace)
            .count())
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // For procedural storage, we delegate directly without enforcing namespace
        // since the backend may handle this differently
        self.inner.store_procedural(messages, session_id).await
    }

    async fn recall_namespaced(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // If the requested namespace matches our own, delegate to the inner memory.
        // Otherwise, return empty results (namespace isolation).
        if namespace == self.namespace {
            self.inner
                .recall_namespaced(&self.namespace, query, limit, session_id, since, until)
                .await
        } else {
            Ok(Vec::new())
        }
    }

    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        importance: Option<f64>,
    ) -> anyhow::Result<()> {
        // Always use the configured namespace, ignoring any provided namespace
        self.inner
            .store_with_metadata(
                key,
                content,
                category,
                session_id,
                Some(&self.namespace),
                importance,
            )
            .await
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        // Only allow purging our own namespace
        if namespace == self.namespace {
            self.inner.purge_namespace(namespace).await
        } else {
            anyhow::bail!(
                "Cannot purge namespace '{}' from isolation context '{}'",
                namespace,
                self.namespace
            )
        }
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        // Purge sessions, but filtered to our namespace
        let entries = self.inner.list(None, Some(session_id)).await?;
        let mut count = 0;
        for entry in entries {
            if entry.namespace == self.namespace {
                if self.inner.forget(&entry.key).await? {
                    count += 1;
                }
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::NoneMemory;

    #[tokio::test]
    async fn namespaced_memory_enforces_namespace_on_store() {
        let inner = Arc::new(NoneMemory::new());
        let namespaced = NamespacedMemory::new(inner, "test_namespace".to_string());

        // Store should succeed
        namespaced
            .store("key1", "value1", MemoryCategory::Core, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn namespaced_memory_prevents_cross_namespace_access() {
        let inner = Arc::new(NoneMemory::new());
        let namespaced = NamespacedMemory::new(inner, "test_namespace".to_string());

        // Try to recall from a different namespace (no-op for NoneMemory)
        let results = namespaced
            .recall_namespaced("other_namespace", "query", 10, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn namespaced_memory_delegates_correctly() {
        let inner = Arc::new(NoneMemory::new());
        let namespaced = NamespacedMemory::new(inner, "test_namespace".to_string());

        assert_eq!(namespaced.name(), "none");
        assert!(namespaced.health_check().await);
        assert_eq!(namespaced.count().await.unwrap(), 0);
    }
}
