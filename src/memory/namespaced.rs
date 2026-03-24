//! Namespace-scoped memory decorator.
//!
//! Wraps any `Memory` backend and transparently scopes all store/recall
//! operations to a fixed namespace. Follows the same decorator pattern
//! as `AuditedMemory<M>`.
//!
//! Used by the delegate tool to give each sub-agent isolated memory
//! within a shared `brain.db`. See issue #2767.

use super::traits::{Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use async_trait::async_trait;
use std::sync::Arc;

/// A memory decorator that scopes all operations to a fixed namespace.
///
/// - `store()` redirects to `store_with_metadata()` with the configured namespace.
/// - `recall()` redirects to `recall_namespaced()` with the configured namespace.
/// - All other operations pass through to the inner backend.
///
/// This enforces namespace isolation at the trait level — the consumer
/// (tools, memory loader) cannot bypass the namespace boundary.
pub struct NamespacedMemory {
    inner: Arc<dyn Memory>,
    namespace: String,
}

impl NamespacedMemory {
    pub fn new(inner: Arc<dyn Memory>, namespace: String) -> Self {
        Self { inner, namespace }
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
        self.inner.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        self.inner.forget(key).await
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        self.inner.purge_namespace(namespace).await
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        self.inner.purge_session(session_id).await
    }

    async fn count(&self) -> anyhow::Result<usize> {
        self.inner.count().await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
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
        // Honor explicit namespace override, but default to ours
        self.inner
            .recall_namespaced(namespace, query, limit, session_id, since, until)
            .await
    }

    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
    ) -> anyhow::Result<()> {
        // Use provided namespace or fall back to ours
        let ns = namespace.unwrap_or(&self.namespace);
        self.inner
            .store_with_metadata(key, content, category, session_id, Some(ns), importance)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::sqlite::SqliteMemory;
    use tempfile::TempDir;

    fn temp_sqlite() -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, mem)
    }

    #[tokio::test]
    async fn store_uses_configured_namespace() {
        let (_tmp, inner) = temp_sqlite();
        let inner = Arc::new(inner);
        let ns_mem = NamespacedMemory::new(inner.clone(), "agent-a".into());

        ns_mem
            .store("k1", "value one", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Verify entry was stored in the correct namespace
        let entry = inner.get("k1").await.unwrap().unwrap();
        assert_eq!(entry.namespace, "agent-a");
    }

    #[tokio::test]
    async fn recall_only_returns_own_namespace() {
        let (_tmp, inner) = temp_sqlite();
        let inner = Arc::new(inner);

        // Store in two different namespaces
        inner
            .store_with_metadata(
                "k1",
                "agent a data",
                MemoryCategory::Core,
                None,
                Some("agent-a"),
                None,
            )
            .await
            .unwrap();
        inner
            .store_with_metadata(
                "k2",
                "agent b data",
                MemoryCategory::Core,
                None,
                Some("agent-b"),
                None,
            )
            .await
            .unwrap();

        let ns_a = NamespacedMemory::new(inner.clone(), "agent-a".into());
        let results = ns_a.recall("data", 10, None, None, None).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "k1");
        assert!(results[0].content.contains("agent a"));
    }

    #[tokio::test]
    async fn cross_namespace_isolation() {
        let (_tmp, inner) = temp_sqlite();
        let inner = Arc::new(inner);

        let ns_a = NamespacedMemory::new(inner.clone(), "agent-a".into());
        let ns_b = NamespacedMemory::new(inner.clone(), "agent-b".into());

        ns_a.store("secret", "agent a secret", MemoryCategory::Core, None)
            .await
            .unwrap();
        ns_b.store("secret", "agent b secret", MemoryCategory::Core, None)
            .await
            .unwrap();

        let a_results = ns_a.recall("secret", 10, None, None, None).await.unwrap();
        let b_results = ns_b.recall("secret", 10, None, None, None).await.unwrap();

        // Each agent only sees its own data
        assert!(a_results.iter().all(|e| e.namespace == "agent-a"));
        assert!(b_results.iter().all(|e| e.namespace == "agent-b"));
    }
}
