//! Runtime memory wrapper bound to one agent.
//!
//! Each agent holds its own per-agent backend instance (selected at
//! agent creation via `[agents.<alias>.memory.backend]`, immutable
//! thereafter). The wrapper sits directly on top of that instance and:
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
//! Cross-backend allowlist entries are rejected at config load. The
//! wrapper only ever sees same-backend sibling UUIDs in its
//! `allowed_agent_ids` set.

use super::traits::{ExportFilter, Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

/// A `Memory` impl that scopes every read and write to a bound agent's
/// UUID + a resolved cross-agent allowlist.
///
/// Construct via [`AgentScopedMemory::new`] at agent-loop entry. The
/// runtime holds one per agent. Non-generic over the inner backend
/// (holds `Arc<dyn Memory>`) so the per-agent factory can hand back a
/// single concrete type regardless of the agent's chosen backend kind.
pub struct AgentScopedMemory {
    /// The wrapped backend. `Arc<dyn Memory>` to slot into the existing
    /// per-install plumbing while the runtime factory hands out one
    /// instance per agent.
    inner: Arc<dyn Memory>,
    /// The bound agent's UUID (from `agents.id`). Stamped on every
    /// write through this wrapper.
    agent_id: String,
    /// Set of agent UUIDs this wrapper recalls from. Always contains
    /// [`Self::agent_id`] (an agent always sees its own rows); any
    /// additional UUIDs come from the configured `read_memory_from`
    /// allowlist resolved at construction.
    allowed_agent_ids: HashSet<String>,
}

impl AgentScopedMemory {
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
        inner: Arc<dyn Memory>,
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

    /// Build a `Vec<&str>` of the allowlist for passing to the
    /// `Memory::recall_for_agents` trait method, which takes a
    /// borrowed slice. Stable iteration order is not required.
    fn allowed_slice(&self) -> Vec<&str> {
        self.allowed_agent_ids.iter().map(String::as_str).collect()
    }
}

#[async_trait]
impl Memory for AgentScopedMemory {
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
        // The wrapper's whole purpose is to make every persisted row
        // attributable to its bound agent. A caller passing an
        // explicit `agent_id` that does not match is a bug; refuse
        // loudly so the misuse is debuggable rather than silently
        // misattributed.
        if let Some(requested) = agent_id
            && requested != self.agent_id
        {
            return Err(anyhow!(
                "AgentScopedMemory bound to agent_id {bound:?} refuses store_with_agent for foreign agent_id {requested:?}; \
                 use a wrapper bound to the target agent",
                bound = self.agent_id,
            ));
        }
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
        // Keyed lookup — the trait does not expose an agent-scoped
        // form, so post-filter on `MemoryEntry::agent_id`. Backends
        // that don't track per-agent attribution (Markdown, NoneMemory,
        // and pre-multi-agent Lucid rows) populate `agent_id` as
        // `None`; treat that as "not in any allowlist" and refuse to
        // surface, so a wrapper-using caller never reads cross-agent
        // rows by guessed key.
        let entry = self.inner.get(key).await?;
        Ok(entry.filter(|e| {
            e.agent_id
                .as_deref()
                .is_some_and(|aid| self.allowed_agent_ids.contains(aid))
        }))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Inner.list returns rows across every agent on the install;
        // post-filter by the bound + allowlisted set so a wrapper-using
        // caller cannot inspect sibling rows it did not opt into via
        // `read_memory_from`.
        let entries = self.inner.list(category, session_id).await?;
        Ok(entries
            .into_iter()
            .filter(|e| {
                e.agent_id
                    .as_deref()
                    .is_some_and(|aid| self.allowed_agent_ids.contains(aid))
            })
            .collect())
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        // Only the bound agent's own rows may be deleted. Sibling rows
        // visible via `read_memory_from` are read-only by design — the
        // allowlist grants recall, never delete. Look up the row,
        // confirm attribution, and refuse anything else.
        let Some(entry) = self.inner.get(key).await? else {
            return Ok(false);
        };
        match entry.agent_id.as_deref() {
            Some(aid) if aid == self.agent_id => self.inner.forget(key).await,
            Some(other) => Err(anyhow!(
                "AgentScopedMemory refuses to forget key {key:?}: row is attributed to agent {other:?}, \
                 not the bound agent {bound:?}",
                bound = self.agent_id,
            )),
            None => Err(anyhow!(
                "AgentScopedMemory refuses to forget key {key:?}: row has no agent attribution \
                 (legacy or backend without per-agent tracking); resolve via an admin Memory handle"
            )),
        }
    }

    async fn count(&self) -> Result<usize> {
        // Scope to the bound + allowlisted agents so a wrapper-using
        // caller does not see the install-wide row total.
        let entries = self.inner.list(None, None).await?;
        Ok(entries
            .into_iter()
            .filter(|e| {
                e.agent_id
                    .as_deref()
                    .is_some_and(|aid| self.allowed_agent_ids.contains(aid))
            })
            .count())
    }

    async fn purge_namespace(&self, namespace: &str) -> Result<usize> {
        // Bulk cross-agent destruction has no agent-scoped form on the
        // trait. Refuse rather than passing through; the operator path
        // for purges is an admin Memory handle, not an agent loop.
        let _ = namespace;
        Err(anyhow!(
            "AgentScopedMemory refuses purge_namespace: cross-agent bulk delete must run through an admin Memory handle, \
             not an agent-scoped wrapper"
        ))
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        // The trait's `purge_session` is install-wide; an agent
        // calling it would clear sibling rows that share the session
        // id. Restrict to the bound agent's own rows by enumerating
        // the session's memberships, filtering, and forgetting one by
        // one. Slower than a bulk DELETE but safe by construction.
        let candidates = self.inner.list(None, Some(session_id)).await?;
        let mut purged = 0;
        for entry in candidates {
            if entry.agent_id.as_deref() == Some(self.agent_id.as_str())
                && self.inner.forget(&entry.key).await?
            {
                purged += 1;
            }
        }
        Ok(purged)
    }

    async fn reindex(&self) -> Result<usize> {
        // Reindex is an admin-shaped op (rebuilds FTS / re-embeds
        // missing vectors). Touching the inner backend here is
        // contained: it does not mutate row attribution or expose
        // cross-agent content to the caller.
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
        // Recall through the agent-scoped recall path so the bound +
        // allowlisted UUIDs filter at the SQL boundary, then
        // post-filter for the namespace match. The default trait impl
        // would route through `recall` which the wrapper has already
        // overridden, but routing explicitly here keeps the read shape
        // visible to anyone tracing the call chain.
        let entries = self
            .recall(query, limit * 2, session_id, since, until)
            .await?;
        Ok(entries
            .into_iter()
            .filter(|e| e.namespace == namespace)
            .take(limit)
            .collect())
    }

    async fn export(&self, filter: &ExportFilter) -> Result<Vec<MemoryEntry>> {
        // Export is the GDPR data-portability path. An agent-scoped
        // export sees only the bound + allowlisted agents' rows. The
        // wrapper's `list` already does the per-agent filtering;
        // delegate to it and apply the rest of the export filter
        // post-fetch.
        let entries = self
            .list(filter.category.as_ref(), filter.session_id.as_deref())
            .await?;
        Ok(entries
            .into_iter()
            .filter(|e| {
                if let Some(ref ns) = filter.namespace
                    && e.namespace != *ns
                {
                    return false;
                }
                if let Some(ref since) = filter.since
                    && e.timestamp.as_str() < since.as_str()
                {
                    return false;
                }
                if let Some(ref until) = filter.until
                    && e.timestamp.as_str() > until.as_str()
                {
                    return false;
                }
                true
            })
            .collect())
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> Result<String> {
        self.inner.ensure_agent_uuid(alias).await
    }
}

impl ::zeroclaw_api::attribution::Attributable for AgentScopedMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(
            ::zeroclaw_api::attribution::MemoryKind::AgentScoped,
        )
    }
    fn alias(&self) -> &str {
        &self.agent_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::SqliteMemory;
    use tempfile::TempDir;

    fn fresh_sqlite() -> (TempDir, Arc<SqliteMemory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    fn as_dyn(inner: Arc<SqliteMemory>) -> Arc<dyn Memory> {
        inner
    }

    /// Insert real agent rows for the supplied aliases and return their
    /// UUIDs. The NOT NULL FK on `memories.agent_id` means tests that
    /// attribute rows to a sibling must use UUIDs that actually exist
    /// in the agents table.
    async fn provision_agents(inner: &Arc<SqliteMemory>, aliases: &[&str]) -> Vec<String> {
        let mut uuids = Vec::with_capacity(aliases.len());
        for alias in aliases {
            uuids.push(inner.ensure_agent_uuid(alias).await.unwrap());
        }
        uuids
    }

    #[tokio::test]
    async fn store_routes_through_store_with_agent_and_persists_attribution() {
        let (_tmp, inner) = fresh_sqlite();
        let alpha = inner.ensure_agent_uuid("alpha").await.unwrap();
        let wrapper = AgentScopedMemory::new(as_dyn(inner.clone()), &alpha, Vec::<String>::new());

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
        let uuids = provision_agents(&inner, &["alpha", "other"]).await;
        let alpha_uuid = &uuids[0];
        let other_uuid = &uuids[1];

        // Pre-seed with rows attributed to the OTHER agent.
        inner
            .store_with_agent(
                "other-key",
                "other-val",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(other_uuid),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, Vec::<String>::new());

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
        let uuids = provision_agents(&inner, &["alpha", "beta"]).await;
        let alpha_uuid = &uuids[0];
        let beta_uuid = &uuids[1];

        inner
            .store_with_agent(
                "sibling-key",
                "sibling-val",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(beta_uuid),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, vec![beta_uuid.clone()]);

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
    async fn get_filters_cross_agent_rows_by_attribution() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "beta"]).await;
        let alpha_uuid = &uuids[0];
        let beta_uuid = &uuids[1];

        // beta writes a row; alpha's wrapper must not see it via get().
        inner
            .store_with_agent(
                "beta-only",
                "secret",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(beta_uuid),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, Vec::<String>::new());

        let hit = wrapper.get("beta-only").await.unwrap();
        assert!(
            hit.is_none(),
            "get must filter out rows attributed to non-allowlisted agents"
        );
    }

    #[tokio::test]
    async fn forget_refuses_to_delete_sibling_rows() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "beta"]).await;
        let alpha_uuid = &uuids[0];
        let beta_uuid = &uuids[1];

        // beta writes a row; alpha's wrapper has read access to beta
        // (via the allowlist) but must still refuse to forget the row.
        inner
            .store_with_agent(
                "beta-row",
                "v",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(beta_uuid),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, vec![beta_uuid.clone()]);

        let err = wrapper
            .forget("beta-row")
            .await
            .expect_err("forget must refuse cross-agent delete even with read allowlist");
        assert!(
            err.to_string().contains("attributed to agent"),
            "expected sibling-attribution refusal, got: {err}"
        );
    }

    #[tokio::test]
    async fn list_filters_to_bound_and_allowlisted_agents() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "beta", "rogue"]).await;
        let alpha_uuid = &uuids[0];
        let beta_uuid = &uuids[1];
        let rogue_uuid = &uuids[2];

        for (key, owner) in [("alpha-row", alpha_uuid), ("rogue-row", rogue_uuid)] {
            inner
                .store_with_agent(
                    key,
                    "v",
                    MemoryCategory::Core,
                    None,
                    None,
                    None,
                    Some(owner),
                )
                .await
                .unwrap();
        }

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, vec![beta_uuid.clone()]);

        let entries = wrapper.list(None, None).await.unwrap();
        assert!(entries.iter().any(|e| e.key == "alpha-row"));
        assert!(
            !entries.iter().any(|e| e.key == "rogue-row"),
            "list must drop rows attributed to non-allowlisted agents"
        );
    }

    #[tokio::test]
    async fn store_with_agent_refuses_foreign_agent_id() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "rogue"]).await;
        let alpha_uuid = &uuids[0];
        let rogue_uuid = &uuids[1];

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, Vec::<String>::new());

        let err = wrapper
            .store_with_agent(
                "k",
                "v",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(rogue_uuid),
            )
            .await
            .expect_err(
                "store_with_agent must refuse a foreign agent_id rather than silently override",
            );
        assert!(
            err.to_string().contains("foreign agent_id"),
            "expected foreign-agent refusal, got: {err}"
        );
    }

    #[tokio::test]
    async fn purge_namespace_is_refused() {
        let (_tmp, inner) = fresh_sqlite();
        let alpha = inner.ensure_agent_uuid("alpha").await.unwrap();
        let wrapper = AgentScopedMemory::new(as_dyn(inner), &alpha, Vec::<String>::new());

        let err = wrapper
            .purge_namespace("default")
            .await
            .expect_err("purge_namespace must be refused on a wrapper");
        assert!(
            err.to_string().contains("admin Memory handle"),
            "expected admin-only refusal, got: {err}"
        );
    }

    #[tokio::test]
    async fn purge_session_only_clears_bound_agents_rows() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "rogue"]).await;
        let alpha_uuid = &uuids[0];
        let rogue_uuid = &uuids[1];

        let session = "shared-session";
        for (key, owner) in [("alpha-row", alpha_uuid), ("rogue-row", rogue_uuid)] {
            inner
                .store_with_agent(
                    key,
                    "v",
                    MemoryCategory::Core,
                    Some(session),
                    None,
                    None,
                    Some(owner),
                )
                .await
                .unwrap();
        }

        let wrapper =
            AgentScopedMemory::new(as_dyn(inner.clone()), alpha_uuid, Vec::<String>::new());
        let purged = wrapper.purge_session(session).await.unwrap();
        assert_eq!(purged, 1, "wrapper must purge only the bound agent's row");

        // rogue's row must survive.
        let rogue_wrapper = AgentScopedMemory::new(as_dyn(inner), rogue_uuid, Vec::<String>::new());
        let rogue_hit = rogue_wrapper.get("rogue-row").await.unwrap();
        assert!(
            rogue_hit.is_some(),
            "purge_session on the alpha wrapper must not touch rogue's rows"
        );
    }

    #[tokio::test]
    async fn recall_for_agents_intersects_caller_allowlist_with_bound_allowlist() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "beta", "rogue"]).await;
        let alpha_uuid = &uuids[0];
        let beta_uuid = &uuids[1];
        let rogue_uuid = &uuids[2];

        inner
            .store_with_agent(
                "rogue-key",
                "rogue-val",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(rogue_uuid),
            )
            .await
            .unwrap();

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha_uuid, vec![beta_uuid.clone()]);

        // Caller asks for a rogue agent that is NOT on the wrapper's
        // bound allowlist. Intersection drops it, so the recall sees
        // no rogue rows.
        let hits = wrapper
            .recall_for_agents(&[rogue_uuid.as_str()], "rogue-key", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            !hits.iter().any(|e| e.key == "rogue-key"),
            "caller allowlist must be intersected, not unioned"
        );
    }
}
