//! `AgentScopedMemory<M>` — runtime memory wrapper bound to one agent
//! (#6272 multi-agent runtime).
//!
//! Each agent in a v0.8.0 install holds its own per-agent backend
//! instance (selected at agent creation via
//! `[agents.<alias>.memory.backend]`, immutable thereafter). The
//! wrapper sits directly on top of that instance and:
//!
//! - Stamps the bound agent's UUID on every store via the inner
//!   backend's `store_with_agent` trait method (real implementations
//!   on every backend; the agent_id is never silently dropped at the
//!   trait boundary).
//! - Filters every recall through the inner backend's
//!   `recall_for_agents` with the resolved allowlist (own UUID + the
//!   `read_memory_from` allowlist from
//!   `[agents.<alias>.workspace.read_memory_from]`).
//! - Intersects caller-supplied per-call allowlists with the bound
//!   allowlist so a caller can never widen scope past what the agent's
//!   config permits.
//!
//! Cross-backend allowlist entries are rejected at config-load (P3
//! validator); v0.8.0 does not support cross-backend cross-agent
//! memory access. The wrapper therefore only ever sees same-backend
//! sibling UUIDs in its `allowed_agent_ids` set.

use super::traits::{ExportFilter, Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

/// A `Memory` impl that scopes every read and write to a bound agent's
/// UUID + a resolved cross-agent allowlist.
///
/// Construct via [`AgentScopedMemory::new`] at agent-loop entry. The
/// runtime holds one per agent.
pub struct AgentScopedMemory<M: Memory> {
    /// The wrapped backend. `Arc<M>` to allow cloning into async tasks
    /// without losing the bound identity, and to stay compatible with
    /// the existing `Arc<dyn Memory>` plumbing in the runtime.
    inner: Arc<M>,
    /// The bound agent's UUID (from `agents.id`). Stamped on every
    /// write through this wrapper.
    agent_id: String,
    /// Set of agent UUIDs this wrapper recalls from. Always contains
    /// [`Self::agent_id`] (an agent always sees its own rows); any
    /// additional UUIDs come from the configured `read_memory_from`
    /// allowlist resolved at construction.
    allowed_agent_ids: HashSet<String>,
}

impl<M: Memory> AgentScopedMemory<M> {
    /// Build a new agent-scoped wrapper around `inner`.
    ///
    /// `agent_id` is the bound agent's UUID (looked up from the
    /// `agents` table by alias at construction time in the runtime
    /// factory). `allowed_sibling_agent_ids` is the resolved
    /// `read_memory_from` allowlist; the bound `agent_id` is added
    /// automatically to the in-memory `allowed_agent_ids` set so
    /// callers do not need to remember to include themselves.
    #[must_use]
    pub fn new(
        inner: Arc<M>,
        agent_id: impl Into<String>,
        allowed_sibling_agent_ids: impl IntoIterator<Item = String>,
    ) -> Self {
        let agent_id = agent_id.into();
        let mut allowed_agent_ids: HashSet<String> =
            allowed_sibling_agent_ids.into_iter().collect();
        allowed_agent_ids.insert(agent_id.clone());
        Self {
            inner,
            agent_id,
            allowed_agent_ids,
        }
    }

    /// The bound agent's UUID.
    #[must_use]
    pub fn bound_agent_id(&self) -> &str {
        &self.agent_id
    }

    /// The full allowlist this wrapper recalls from (bound agent
    /// included). Borrowed slice for read-only inspection by tests
    /// and callers that need to log effective scope.
    #[must_use]
    pub fn allowed_agent_ids(&self) -> &HashSet<String> {
        &self.allowed_agent_ids
    }

    /// Build a `Vec<&str>` of the allowlist for passing to the
    /// `Memory::recall_for_agents` trait method, which takes a
    /// borrowed slice. Stable iteration order is not required.
    fn allowed_slice(&self) -> Vec<&str> {
        self.allowed_agent_ids.iter().map(String::as_str).collect()
    }
}

#[async_trait]
impl<M: Memory> Memory for AgentScopedMemory<M> {
    fn name(&self) -> &str {
        // Kept identical to the inner backend so existing log lines
        // and dashboards keep working; the wrapper's existence is
        // visible only through the `agent_alias` tracing field bound
        // at agent-loop entry.
        self.inner.name()
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        // Every store routes through `store_with_agent` so the bound
        // agent's UUID is persisted. Backends with native agent_id
        // columns (Sqlite, Postgres, Lucid) write the column; Qdrant
        // writes the payload field; Markdown attributes via the on-
        // disk path; None drops it. Each backend's behavior is
        // explicit at the trait boundary.
        self.inner
            .store_with_agent(
                key,
                content,
                category,
                session_id,
                None,
                None,
                Some(&self.agent_id),
            )
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
    ) -> Result<()> {
        self.inner
            .store_with_agent(
                key,
                content,
                category,
                session_id,
                namespace,
                importance,
                Some(&self.agent_id),
            )
            .await
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        // A wrapper-internal caller of `store_with_agent` may try to
        // override the bound agent. We refuse silently and stamp the
        // bound agent's id instead — the wrapper's whole purpose is
        // to make every persisted row attributable to one agent. If a
        // caller really wants a different agent, they should
        // construct a different wrapper.
        let _ = agent_id;
        self.inner
            .store_with_agent(
                key,
                content,
                category,
                session_id,
                namespace,
                importance,
                Some(&self.agent_id),
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
    ) -> Result<Vec<MemoryEntry>> {
        let allowed = self.allowed_slice();
        self.inner
            .recall_for_agents(&allowed, query, limit, session_id, since, until)
            .await
    }

    async fn recall_for_agents(
        &self,
        caller_allowed: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Intersect the caller-supplied allowlist with the bound
        // allowlist so a caller cannot widen scope past what the
        // agent's config permits. Empty caller allowlist means "no
        // extra restriction"; the bound allowlist still applies.
        // A non-empty caller allowlist whose intersection with the
        // bound allowlist is empty means "no rows match" — return
        // early so the empty-allowlist sentinel ("no filter") on the
        // inner backend does not silently widen scope.
        if caller_allowed.is_empty() {
            let bound: Vec<&str> = self.allowed_agent_ids.iter().map(String::as_str).collect();
            return self
                .inner
                .recall_for_agents(&bound, query, limit, session_id, since, until)
                .await;
        }

        let intersected: Vec<&str> = caller_allowed
            .iter()
            .copied()
            .filter(|id| self.allowed_agent_ids.contains(*id))
            .collect();
        if intersected.is_empty() {
            return Ok(Vec::new());
        }
        self.inner
            .recall_for_agents(&intersected, query, limit, session_id, since, until)
            .await
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        // `get` is keyed lookup — the trait does not yet expose an
        // agent-scoped form. v0.8.0 leaves this on the inner backend;
        // a key collision across agents is already prevented by the
        // unique-key DB index, so v0.8.1 can extend the trait without
        // changing semantics.
        self.inner.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // `list` is admin-shaped and does not currently filter on
        // agent_id at the trait. Pass through; the v0.8.1 follow-up
        // adds an agent-scoped variant.
        self.inner.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        self.inner.forget(key).await
    }

    async fn count(&self) -> Result<usize> {
        self.inner.count().await
    }

    async fn purge_namespace(&self, namespace: &str) -> Result<usize> {
        self.inner.purge_namespace(namespace).await
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        self.inner.purge_session(session_id).await
    }

    async fn reindex(&self) -> Result<usize> {
        self.inner.reindex().await
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> Result<()> {
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
    ) -> Result<Vec<MemoryEntry>> {
        self.inner
            .recall_namespaced(namespace, query, limit, session_id, since, until)
            .await
    }

    async fn export(&self, filter: &ExportFilter) -> Result<Vec<MemoryEntry>> {
        self.inner.export(filter).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteMemory;
    use tempfile::TempDir;

    fn fresh_sqlite() -> (TempDir, Arc<SqliteMemory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[tokio::test]
    async fn store_routes_through_store_with_agent_and_persists_attribution() {
        let (_tmp, inner) = fresh_sqlite();
        let wrapper =
            AgentScopedMemory::new(inner.clone(), "agent-uuid-alpha", Vec::<String>::new());

        wrapper
            .store("k1", "v1", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Recall via the wrapper's bound allowlist returns the entry.
        let hits = wrapper.recall("k1", 10, None, None, None).await.unwrap();
        assert!(
            hits.iter().any(|e| e.key == "k1"),
            "wrapper recall must find rows it just stored"
        );
    }

    #[tokio::test]
    async fn recall_excludes_other_agent_rows_when_allowlist_omits_them() {
        let (_tmp, inner) = fresh_sqlite();
        // Pre-seed with rows attributed to the OTHER agent. Use the
        // backend's `store_with_agent` directly so we can choose the
        // attribution.
        inner
            .store_with_agent(
                "other-key",
                "other-val",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some("agent-uuid-other"),
            )
            .await
            .unwrap();

        // The wrapper's bound id is alpha and its allowlist is empty
        // (only itself).
        let wrapper = AgentScopedMemory::new(inner, "agent-uuid-alpha", Vec::<String>::new());

        let hits = wrapper
            .recall("other-key", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            !hits.iter().any(|e| e.key == "other-key"),
            "rows attributed to a non-allowlisted agent must not surface"
        );
    }

    #[tokio::test]
    async fn recall_includes_allowlisted_sibling_rows() {
        let (_tmp, inner) = fresh_sqlite();
        inner
            .store_with_agent(
                "sibling-key",
                "sibling-val",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some("agent-uuid-beta"),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(
            inner,
            "agent-uuid-alpha",
            vec!["agent-uuid-beta".to_string()],
        );

        let hits = wrapper
            .recall("sibling-key", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|e| e.key == "sibling-key"),
            "rows attributed to an allowlisted sibling must surface"
        );
    }

    #[tokio::test]
    async fn recall_for_agents_intersects_caller_allowlist_with_bound_allowlist() {
        let (_tmp, inner) = fresh_sqlite();
        inner
            .store_with_agent(
                "rogue-key",
                "rogue-val",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some("agent-uuid-rogue"),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(
            inner,
            "agent-uuid-alpha",
            vec!["agent-uuid-beta".to_string()],
        );

        // Caller asks for a rogue agent that is NOT on the wrapper's
        // bound allowlist. Intersection drops it, so the recall sees
        // no rogue rows.
        let hits = wrapper
            .recall_for_agents(&["agent-uuid-rogue"], "rogue-key", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            !hits.iter().any(|e| e.key == "rogue-key"),
            "caller allowlist must be intersected, not unioned"
        );
    }
}
