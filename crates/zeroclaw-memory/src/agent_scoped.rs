use super::traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, ProceduralMessage, StoreOptions,
};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::Arc;

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

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        // Transparent wrapper: forward the embedder refresh to the wrapped
        // per-agent backend so an active agent's memory stops using a stale
        // endpoint/key after a provider-profile `config/set`
        self.inner
            .refresh_embedder(model_provider, api_key, model, dimensions);
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
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

    async fn store_with_options(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
    ) -> Result<()> {
        self.inner
            .store_with_options_and_agent(
                key,
                content,
                category,
                session_id,
                options,
                Some(&self.agent_id),
            )
            .await
    }

    async fn store_with_options_and_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
        agent_id: Option<&str>,
    ) -> Result<()> {
        if let Some(requested) = agent_id
            && requested != self.agent_id
        {
            anyhow::bail!(
                "AgentScopedMemory refuses store_with_options_and_agent for foreign agent_id; use a wrapper bound to the target agent"
            );
        }
        self.store_with_options(key, content, category, session_id, options)
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
        if let Some(requested) = agent_id
            && requested != self.agent_id
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "bound_agent": self.agent_id,
                        "requested_agent": requested,
                        "key": key,
                    })),
                "store_with_agent refused: foreign agent_id"
            );
            anyhow::bail!(
                "AgentScopedMemory refuses store_with_agent for foreign agent_id; use a wrapper bound to the target agent"
            );
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
        if let Some(own) = self.inner.get_for_agent(key, &self.agent_id).await? {
            return Ok(Some(own));
        }
        for sibling in &self.allowed_agent_ids {
            if sibling == &self.agent_id {
                continue;
            }
            if let Some(hit) = self.inner.get_for_agent(key, sibling).await? {
                return Ok(Some(hit));
            }
        }
        Ok(None)
    }

    async fn get_for_agent(&self, key: &str, agent_id: &str) -> Result<Option<MemoryEntry>> {
        if agent_id != self.agent_id && !self.allowed_agent_ids.iter().any(|a| a == agent_id) {
            return Ok(None);
        }
        self.inner.get_for_agent(key, agent_id).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
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
        if self.inner.forget_for_agent(key, &self.agent_id).await? {
            return Ok(true);
        }
        match self.inner.get(key).await? {
            None => Ok(false),
            Some(entry) => match entry.agent_id.as_deref() {
                Some(other) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "key": key,
                                "row_agent": other,
                                "bound_agent": self.agent_id,
                            })),
                        "forget refused: row attributed to a different agent"
                    );
                    anyhow::bail!(
                        "AgentScopedMemory refuses to forget cross-agent row: key attributed to agent other than the bound agent"
                    );
                }
                None => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "key": key,
                                "bound_agent": self.agent_id,
                            })),
                        "forget refused: row has no agent attribution"
                    );
                    anyhow::bail!(
                        "AgentScopedMemory refuses to forget unattributed row: legacy or backend without per-agent tracking; resolve via an admin Memory handle"
                    );
                }
            },
        }
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> Result<bool> {
        // Only the bound agent can delete its own row through the
        // wrapper. Allowlist grants recall, never delete.
        if agent_id != self.agent_id {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "key": key,
                        "row_agent": agent_id,
                        "bound_agent": self.agent_id,
                    })),
                "forget_for_agent refused: cross-agent delete through wrapper"
            );
            anyhow::bail!(
                "AgentScopedMemory refuses cross-agent forget_for_agent: bound agent and target agent differ"
            );
        }
        self.inner.forget_for_agent(key, agent_id).await
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
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "namespace": namespace,
                    "bound_agent": self.agent_id,
                })),
            "purge_namespace refused: cross-agent bulk delete requires an admin Memory handle"
        );
        anyhow::bail!(
            "AgentScopedMemory refuses purge_namespace: cross-agent bulk delete must run through an admin Memory handle"
        );
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        // Bulk session deletes must be scoped by both session and bound
        // agent at the backend boundary. Listing a session and deleting by
        // `(key, agent_id)` can delete the bound agent's row from a
        // different session when keys collide.
        self.inner
            .purge_session_for_agent(session_id, &self.agent_id)
            .await
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
    use crate::embeddings::EmbeddingProvider;
    use crate::sqlite::SqliteMemory;
    use crate::traits::{MemoryKind, SemanticSubtype};
    use tempfile::TempDir;
    use zeroclaw_config::schema::SearchMode;

    fn fresh_sqlite() -> (TempDir, Arc<SqliteMemory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    /// The query text alone maps to the live vector axis. Stored rows stay
    /// FTS-only, which makes the test exercise FTS normalization in the real
    /// `AgentScopedMemory -> recall_for_agents -> SqliteMemory` path.
    struct SessionScopeEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for SessionScopeEmbedding {
        fn name(&self) -> &str {
            "session-scope"
        }

        fn dimensions(&self) -> usize {
            2
        }

        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    if *text == "archive axis bridge cipher delta ember frost glyph" {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
    }

    fn fresh_live_sqlite() -> (TempDir, Arc<SqliteMemory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::with_embedder(
            "test",
            tmp.path(),
            Arc::new(SessionScopeEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::default(),
        )
        .unwrap();
        (tmp, Arc::new(mem))
    }

    fn as_dyn(inner: Arc<SqliteMemory>) -> Arc<dyn Memory> {
        inner
    }

    async fn provision_agents(inner: &Arc<SqliteMemory>, aliases: &[&str]) -> Vec<String> {
        let mut uuids = Vec::with_capacity(aliases.len());
        for alias in aliases {
            uuids.push(inner.ensure_agent_uuid(alias).await.unwrap());
        }
        uuids
    }

    #[test]
    fn refresh_embedder_forwards_to_inner_backend() {
        let (_tmp, inner) = fresh_sqlite();
        let wrapper =
            AgentScopedMemory::new(as_dyn(inner.clone()), "agent-1", Vec::<String>::new());
        assert_eq!(inner.embedder_dimensions(), 0);

        Memory::refresh_embedder(
            &wrapper,
            "openai",
            Some("sk-test"),
            "text-embedding-3-small",
            1536,
        );

        assert_eq!(
            inner.embedder_dimensions(),
            1536,
            "AgentScopedMemory must forward refresh_embedder to the wrapped backend"
        );
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
    async fn store_with_options_preserves_full_metadata_and_attribution() {
        let (_tmp, inner) = fresh_sqlite();
        let alpha = inner.ensure_agent_uuid("alpha").await.unwrap();
        let wrapper = AgentScopedMemory::new(as_dyn(inner.clone()), &alpha, Vec::<String>::new());

        wrapper
            .store_with_options(
                "decision",
                "Use staged rollout",
                MemoryCategory::Core,
                Some("session-1"),
                StoreOptions {
                    namespace: Some("operations".into()),
                    importance: Some(0.9),
                    kind: Some(MemoryKind::Semantic(SemanticSubtype::Decision)),
                    pinned: true,
                    tenant_id: Some("tenant-a".into()),
                },
            )
            .await
            .unwrap();

        let entry = inner
            .get_for_agent("decision", &alpha)
            .await
            .unwrap()
            .expect("bound agent row should persist");
        assert_eq!(entry.agent_id.as_deref(), Some(alpha.as_str()));
        assert_eq!(entry.namespace, "operations");
        assert_eq!(entry.importance, Some(0.9));
        assert_eq!(
            entry.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Decision))
        );
        assert!(entry.pinned);
        assert_eq!(entry.tenant_id.as_deref(), Some("tenant-a"));
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
    async fn live_vector_recall_scopes_fts_before_ranking_and_keeps_allowed_agents() {
        let (_tmp, inner) = fresh_live_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "beta", "foreign"]).await;
        let alpha = &uuids[0];
        let beta = &uuids[1];
        let foreign = &uuids[2];
        let query = "archive axis bridge cipher delta ember frost glyph";
        let excluded_session_match = format!("{} ", query).repeat(16);
        let excluded_agent_match = format!("{} ", query).repeat(20);

        inner
            .store_with_agent(
                "current_eligible",
                "archive",
                MemoryCategory::Conversation,
                Some("current-session"),
                None,
                None,
                Some(alpha),
            )
            .await
            .unwrap();
        inner
            .store_with_agent(
                "other_session_stronger",
                &excluded_session_match,
                MemoryCategory::Conversation,
                Some("other-session"),
                None,
                None,
                Some(alpha),
            )
            .await
            .unwrap();
        for (key, category) in [
            ("foreign_global_core", MemoryCategory::Core),
            ("foreign_global_daily", MemoryCategory::Daily),
        ] {
            inner
                .store_with_agent(
                    key,
                    "archive foreign note",
                    category,
                    None,
                    None,
                    None,
                    Some(foreign),
                )
                .await
                .unwrap();
        }
        inner
            .store_with_agent(
                "foreign_agent_stronger",
                &excluded_agent_match,
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(foreign),
            )
            .await
            .unwrap();
        inner
            .store_with_agent(
                "allowlisted_global",
                "archive shared note",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(beta),
            )
            .await
            .unwrap();

        let current_id = inner
            .get_for_agent("current_eligible", alpha)
            .await
            .unwrap()
            .expect("current eligible row must exist")
            .id;
        let excluded_id = inner
            .get_for_agent("other_session_stronger", alpha)
            .await
            .unwrap()
            .expect("other-session row must exist")
            .id;
        let foreign_agent_id = inner
            .get_for_agent("foreign_agent_stronger", foreign)
            .await
            .unwrap()
            .expect("foreign-agent row must exist")
            .id;
        let unscoped_fts = {
            let conn = inner.connection().lock();
            SqliteMemory::fts5_search(&conn, query, 10).unwrap()
        };
        let eligible_raw_score = unscoped_fts
            .iter()
            .find(|(id, _)| id == &current_id)
            .map(|(_, score)| *score)
            .expect("current-session row must match the unscoped FTS query");
        let excluded_raw_score = unscoped_fts
            .iter()
            .find(|(id, _)| id == &excluded_id)
            .map(|(_, score)| *score)
            .expect("other-session row must match the unscoped FTS query");
        let foreign_agent_raw_score = unscoped_fts
            .iter()
            .find(|(id, _)| id == &foreign_agent_id)
            .map(|(_, score)| *score)
            .expect("foreign-agent row must match the unscoped FTS query");
        assert!(
            excluded_raw_score > eligible_raw_score * 2.5,
            "the excluded row must be strong enough to reproduce batch normalization pressure: excluded={excluded_raw_score}, eligible={eligible_raw_score}"
        );
        assert!(
            foreign_agent_raw_score > eligible_raw_score * 2.5,
            "the foreign-agent row must be strong enough to reproduce agent-scope normalization pressure: foreign={foreign_agent_raw_score}, eligible={eligible_raw_score}"
        );

        let wrapper = AgentScopedMemory::new(as_dyn(inner), alpha, vec![beta.clone()]);
        let hits = wrapper
            .recall(query, 10, Some("current-session"), None, None)
            .await
            .unwrap();
        let keys: Vec<&str> = hits.iter().map(|entry| entry.key.as_str()).collect();
        let eligible_score = hits
            .iter()
            .find(|entry| entry.key == "current_eligible")
            .and_then(|entry| entry.score)
            .expect("the current-session FTS candidate must be recalled");

        assert!(
            eligible_score >= 0.4,
            "excluded session/agent rows must not depress the best eligible FTS score below the default relevance floor: {eligible_score}"
        );
        assert!(
            keys.contains(&"allowlisted_global"),
            "an explicitly allowlisted sibling's durable global row must remain visible: {keys:?}"
        );
        assert!(
            !keys.contains(&"other_session_stronger"),
            "rows bound to another session must not surface: {keys:?}"
        );
        assert!(
            !keys.contains(&"foreign_agent_stronger"),
            "rows bound to a non-allowlisted agent must not surface or depress allowed scores: {keys:?}"
        );
        assert!(
            !keys.contains(&"foreign_global_core") && !keys.contains(&"foreign_global_daily"),
            "foreign agents' durable global rows must remain outside the wrapper allowlist: {keys:?}"
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
    async fn purge_session_deletes_only_bound_agent_rows_in_that_session() {
        let (_tmp, inner) = fresh_sqlite();
        let uuids = provision_agents(&inner, &["alpha", "beta"]).await;
        let alpha_uuid = &uuids[0];
        let beta_uuid = &uuids[1];

        inner
            .store_with_agent(
                "shared-key",
                "alpha other session",
                MemoryCategory::Core,
                Some("other-session"),
                None,
                None,
                Some(alpha_uuid),
            )
            .await
            .unwrap();
        inner
            .store_with_agent(
                "shared-key",
                "beta target session",
                MemoryCategory::Core,
                Some("target-session"),
                None,
                None,
                Some(beta_uuid),
            )
            .await
            .unwrap();
        inner
            .store_with_agent(
                "alpha-target",
                "alpha target session",
                MemoryCategory::Core,
                Some("target-session"),
                None,
                None,
                Some(alpha_uuid),
            )
            .await
            .unwrap();

        let wrapper =
            AgentScopedMemory::new(as_dyn(inner.clone()), alpha_uuid, vec![beta_uuid.clone()]);

        let purged = wrapper.purge_session("target-session").await.unwrap();
        assert_eq!(purged, 1, "only alpha's row in target-session is deleted");
        assert!(
            inner
                .get_for_agent("shared-key", alpha_uuid)
                .await
                .unwrap()
                .is_some(),
            "same-key alpha row in another session must survive"
        );
        assert!(
            inner
                .get_for_agent("shared-key", beta_uuid)
                .await
                .unwrap()
                .is_some(),
            "sibling row in target-session must survive"
        );
        assert!(
            inner
                .get_for_agent("alpha-target", alpha_uuid)
                .await
                .unwrap()
                .is_none(),
            "bound agent row in target-session must be deleted"
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
