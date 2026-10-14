//! A memory handle pinned to one principal's PRIVATE plane (RFC 7141).
//!
//! An agent session owned by a principal receives its memory through this
//! wrapper: every ordinary `Memory` operation the session's tools and loop
//! issue is routed to the backend's `*_for_principal` form with the owner's
//! composite scope, so a scoped session can neither read nor write the
//! shared plane, and nothing it stores is visible to legacy callers. A
//! backend that has not implemented principal scoping fails every operation
//! closed through the trait defaults, which is the intended posture until it
//! does. Operations with no private-plane meaning (agent-wide maintenance,
//! consolidation, superseding, renames) refuse rather than fall through.
//!
//! The wrapper is deliberately not an authorization mechanism: the session
//! owner was authorized before the handle was built, and the storage
//! predicates carry the owner on every statement.

use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::attribution::{Attributable, Role, ToolProvenance};
use zeroclaw_api::memory_traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, PrincipalScope,
};

/// A `Memory` handle whose ordinary operations act on one principal's
/// private plane.
pub struct PrincipalPlaneMemory {
    inner: Arc<dyn Memory>,
    scope: PrincipalScope,
    name: String,
}

impl Attributable for PrincipalPlaneMemory {
    fn role(&self) -> Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
    fn tool_provenance(&self) -> ToolProvenance {
        self.inner.tool_provenance()
    }
}

impl PrincipalPlaneMemory {
    #[must_use]
    pub fn new(inner: Arc<dyn Memory>, scope: PrincipalScope) -> Self {
        let name = format!("principal:{}", inner.name());
        Self { inner, scope, name }
    }

    /// The scope every routed operation carries.
    #[must_use]
    pub fn scope(&self) -> &PrincipalScope {
        &self.scope
    }

    fn scoped_to_agent(&self, agent: Option<&str>) -> PrincipalScope {
        match agent {
            Some(alias) => self.scope.clone().with_agent(Some(alias.to_string())),
            None => self.scope.clone(),
        }
    }

    fn unavailable(operation: &str) -> anyhow::Error {
        anyhow::Error::msg(format!(
            "{operation} is not available on a principal's private memory plane"
        ))
    }
}

#[async_trait]
impl Memory for PrincipalPlaneMemory {
    fn name(&self) -> &str {
        &self.name
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.inner
            .store_for_principal(&self.scope, key, content, category, session_id)
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
            .recall_for_principal(&self.scope, query, limit, session_id, since, until)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        self.inner.get_for_principal(&self.scope, key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner
            .list_for_principal(&self.scope, category, session_id)
            .await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        self.inner.forget_for_principal(&self.scope, key).await
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        // The agent dimension composes with the owner: the row removed is the
        // owner's row under that agent, never anyone else's.
        self.inner
            .forget_for_principal(&self.scoped_to_agent(Some(agent_id)), key)
            .await
    }

    async fn count(&self) -> anyhow::Result<usize> {
        self.inner.count_for_principal(&self.scope).await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        _importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // Importance is not carried by the private plane; the row keeps the
        // plane's default. Namespace and agent compose into the scope.
        let mut scope = self.scoped_to_agent(agent_id);
        if let Some(namespace) = namespace {
            scope = scope.with_namespace(Some(namespace.to_string()));
        }
        self.inner
            .store_for_principal(&scope, key, content, category, session_id)
            .await
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Peer-agent grants are a shared-plane concept. On the private plane
        // the owner's rows under each named agent are the whole universe.
        let mut results = Vec::new();
        for agent in allowed_agent_ids {
            let scope = self.scoped_to_agent(Some(agent));
            results.extend(
                self.inner
                    .recall_for_principal(&scope, query, limit, session_id, since, until)
                    .await?,
            );
        }
        results.truncate(limit);
        Ok(results)
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
        let scope = self
            .scope
            .clone()
            .with_namespace(Some(namespace.to_string()));
        self.inner
            .recall_for_principal(&scope, query, limit, session_id, since, until)
            .await
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        self.inner
            .purge_namespace_for_principal(&self.scope, namespace)
            .await
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        self.inner
            .purge_session_for_principal(&self.scope, session_id)
            .await
    }

    async fn purge_session_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<usize> {
        self.inner
            .purge_session_for_principal(&self.scoped_to_agent(Some(agent_id)), session_id)
            .await
    }

    async fn purge_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
        Err(Self::unavailable("purging an agent's memory"))
    }

    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner.export_for_principal(&self.scope, filter).await
    }

    async fn export_agent(&self, agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner
            .export_for_principal(
                &self.scoped_to_agent(Some(agent_alias)),
                &ExportFilter::default(),
            )
            .await
    }

    async fn rename_agent(&self, _from: &str, _to: &str) -> anyhow::Result<usize> {
        Err(Self::unavailable("renaming an agent"))
    }

    async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        self.inner
            .count_for_principal(&self.scoped_to_agent(Some(agent_alias)))
            .await
    }

    // The explicit principal forms pass through with the caller's scope: a
    // caller that names a scope is already speaking the private-plane API.

    async fn store_for_principal(
        &self,
        scope: &PrincipalScope,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.inner
            .store_for_principal(scope, key, content, category, session_id)
            .await
    }

    async fn recall_for_principal(
        &self,
        scope: &PrincipalScope,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner
            .recall_for_principal(scope, query, limit, session_id, since, until)
            .await
    }

    async fn list_for_principal(
        &self,
        scope: &PrincipalScope,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner
            .list_for_principal(scope, category, session_id)
            .await
    }

    async fn get_for_principal(
        &self,
        scope: &PrincipalScope,
        key: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        self.inner.get_for_principal(scope, key).await
    }

    async fn forget_for_principal(
        &self,
        scope: &PrincipalScope,
        key: &str,
    ) -> anyhow::Result<bool> {
        self.inner.forget_for_principal(scope, key).await
    }

    async fn count_for_principal(&self, scope: &PrincipalScope) -> anyhow::Result<usize> {
        self.inner.count_for_principal(scope).await
    }

    async fn export_for_principal(
        &self,
        scope: &PrincipalScope,
        filter: &ExportFilter,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner.export_for_principal(scope, filter).await
    }

    async fn purge_namespace_for_principal(
        &self,
        scope: &PrincipalScope,
        namespace: &str,
    ) -> anyhow::Result<usize> {
        self.inner
            .purge_namespace_for_principal(scope, namespace)
            .await
    }

    async fn purge_session_for_principal(
        &self,
        scope: &PrincipalScope,
        session_id: &str,
    ) -> anyhow::Result<usize> {
        self.inner
            .purge_session_for_principal(scope, session_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteMemory;

    fn sqlite() -> (tempfile::TempDir, Arc<dyn Memory>) {
        let tmp = tempfile::tempdir().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).expect("sqlite memory");
        (tmp, Arc::new(mem))
    }

    #[tokio::test]
    async fn ordinary_operations_land_on_the_owners_private_plane_only() {
        let (_tmp, inner) = sqlite();
        let alice =
            PrincipalPlaneMemory::new(Arc::clone(&inner), PrincipalScope::new("user:alice"));
        alice
            .store("note", "alice-private", MemoryCategory::Core, Some("s1"))
            .await
            .unwrap();

        // Visible through the handle, under the caller's key, owner attached.
        let got = alice
            .get("note")
            .await
            .unwrap()
            .expect("alice reads her row");
        assert_eq!(got.content, "alice-private");
        assert_eq!(got.key, "note");
        assert_eq!(got.principal_id.as_deref(), Some("user:alice"));
        assert_eq!(alice.count().await.unwrap(), 1);
        assert_eq!(
            alice
                .list(Some(&MemoryCategory::Core), None)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            alice
                .list(Some(&MemoryCategory::Daily), None)
                .await
                .unwrap()
                .is_empty(),
            "the category predicate is applied in storage"
        );

        // Invisible to the legacy handle and to another principal's handle.
        assert!(inner.get("note").await.unwrap().is_none());
        assert!(inner.list(None, None).await.unwrap().is_empty());
        assert!(
            inner
                .recall("private", 10, None, None, None)
                .await
                .unwrap()
                .is_empty()
        );
        let bob = PrincipalPlaneMemory::new(Arc::clone(&inner), PrincipalScope::new("user:bob"));
        assert!(bob.get("note").await.unwrap().is_none());
        assert_eq!(bob.count().await.unwrap(), 0);

        // Exports through the handle are the owner's rows, owner preserved;
        // the legacy export never carries them.
        let exported = alice.export(&ExportFilter::default()).await.unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].principal_id.as_deref(), Some("user:alice"));
        assert!(
            inner
                .export(&ExportFilter::default())
                .await
                .unwrap()
                .is_empty()
        );

        // Bulk deletes through the handle stay inside the plane.
        bob.store("note", "bob-private", MemoryCategory::Core, Some("s1"))
            .await
            .unwrap();
        assert_eq!(alice.purge_session("s1").await.unwrap(), 1);
        assert!(alice.get("note").await.unwrap().is_none());
        assert_eq!(
            bob.get("note").await.unwrap().map(|e| e.content),
            Some("bob-private".to_string()),
            "a purge through one owner's handle never reaches another's rows"
        );
    }

    #[tokio::test]
    async fn the_agent_dimension_composes_with_the_owner() {
        let (_tmp, inner) = sqlite();
        inner.ensure_agent_uuid("ops").await.unwrap();
        let on_default =
            PrincipalPlaneMemory::new(Arc::clone(&inner), PrincipalScope::new("user:alice"));
        let on_ops = PrincipalPlaneMemory::new(
            Arc::clone(&inner),
            PrincipalScope::new("user:alice").with_agent(Some("ops".into())),
        );
        on_default
            .store("plan", "default-agent-plan", MemoryCategory::Core, None)
            .await
            .unwrap();
        on_ops
            .store("plan", "ops-agent-plan", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(
            on_default.get("plan").await.unwrap().map(|e| e.content),
            Some("default-agent-plan".to_string())
        );
        assert_eq!(
            on_ops.get("plan").await.unwrap().map(|e| e.content),
            Some("ops-agent-plan".to_string())
        );
        assert_eq!(on_default.count().await.unwrap(), 1);
        assert_eq!(on_ops.count().await.unwrap(), 1);
        assert_eq!(on_default.count_agent("ops").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_backend_without_private_support_fails_closed_through_the_handle() {
        struct Legacy;
        impl Attributable for Legacy {
            fn role(&self) -> Role {
                Role::Memory(zeroclaw_api::attribution::MemoryKind::InMemory)
            }
            fn alias(&self) -> &str {
                "legacy"
            }
        }
        #[async_trait]
        impl Memory for Legacy {
            fn name(&self) -> &str {
                "legacy"
            }
            async fn store(
                &self,
                _key: &str,
                _content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
            ) -> anyhow::Result<()> {
                panic!("the shared store must never be reached through the private handle")
            }
            async fn recall(
                &self,
                _query: &str,
                _limit: usize,
                _session_id: Option<&str>,
                _since: Option<&str>,
                _until: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                panic!("the shared recall must never be reached through the private handle")
            }
            async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
                panic!("the shared get must never be reached through the private handle")
            }
            async fn list(
                &self,
                _category: Option<&MemoryCategory>,
                _session_id: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                panic!("the shared list must never be reached through the private handle")
            }
            async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
                panic!("the shared forget must never be reached through the private handle")
            }
            async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
                panic!("the shared forget must never be reached through the private handle")
            }
            async fn count(&self) -> anyhow::Result<usize> {
                Ok(0)
            }
            async fn health_check(&self) -> bool {
                true
            }
            async fn store_with_agent(
                &self,
                _key: &str,
                _content: &str,
                _category: MemoryCategory,
                _session_id: Option<&str>,
                _namespace: Option<&str>,
                _importance: Option<f64>,
                _agent_id: Option<&str>,
            ) -> anyhow::Result<()> {
                panic!("the shared store must never be reached through the private handle")
            }
            async fn recall_for_agents(
                &self,
                _allowed_agent_ids: &[&str],
                _query: &str,
                _limit: usize,
                _session_id: Option<&str>,
                _since: Option<&str>,
                _until: Option<&str>,
            ) -> anyhow::Result<Vec<MemoryEntry>> {
                panic!("the shared recall must never be reached through the private handle")
            }
        }
        let handle = PrincipalPlaneMemory::new(Arc::new(Legacy), PrincipalScope::new("user:alice"));
        let err = handle
            .store("k", "v", MemoryCategory::Core, None)
            .await
            .expect_err("no private support means no write");
        assert!(
            err.to_string()
                .contains("does not support principal-scoped memory")
        );
        assert!(handle.get("k").await.is_err());
        assert!(handle.recall("v", 5, None, None, None).await.is_err());
        assert!(handle.list(None, None).await.is_err());
        assert!(handle.forget("k").await.is_err());
        assert!(handle.purge_session("s").await.is_err());
        assert!(handle.export(&ExportFilter::default()).await.is_err());
    }
}
