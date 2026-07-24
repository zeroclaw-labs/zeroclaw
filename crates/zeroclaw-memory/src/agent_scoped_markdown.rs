//! Cross-agent path-walk variant for Markdown-backed agents.

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub struct MarkdownPeer {
    pub alias: String,
    pub memory: Arc<dyn Memory>,
}

/// Composed Markdown memory for one agent: own backend plus the
/// resolved peer set. Stores write only to the bound agent; recalls
/// union across own + peers with per-row alias attribution.
pub struct AgentScopedMarkdownMemory {
    /// The bound agent's alias. Used for attribution on the agent's
    /// own rows in the merged recall output.
    own_alias: String,
    /// The bound agent's MarkdownMemory pointing at
    /// `<install>/agents/<own_alias>/workspace/`.
    own: Arc<dyn Memory>,
    /// Resolved sibling agents this wrapper recalls from. Empty means
    /// jailed — the agent only sees its own rows. Same-backend
    /// invariant: every peer here is also Markdown-backed (the
    /// cross-reference validator rejects mismatched-backend allowlist
    /// entries at config load).
    peers: Vec<MarkdownPeer>,
}

impl AgentScopedMarkdownMemory {
    pub fn new(
        own_alias: impl Into<String>,
        own: Arc<dyn Memory>,
        peers: Vec<MarkdownPeer>,
    ) -> Self {
        Self {
            own_alias: own_alias.into(),
            own,
            peers,
        }
    }

    fn attribute(alias: &str, mut entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
        for entry in &mut entries {
            entry.key = format!("[{alias}] {}", entry.key);
            entry.agent_alias = Some(alias.to_string());
            entry.agent_id = Some(alias.to_string());
        }
        entries
    }

    /// Lighter-weight variant for non-merged reads (own-only `get`,
    /// `list`): set attribution without rewriting the key. Used by
    /// `get` / `list` where the row already comes from the bound
    /// agent's own backend and no `[alias]` namespacing is needed.
    fn stamp_attribution(alias: &str, mut entries: Vec<MemoryEntry>) -> Vec<MemoryEntry> {
        for entry in &mut entries {
            entry.agent_alias = Some(alias.to_string());
            entry.agent_id = Some(alias.to_string());
        }
        entries
    }
}

#[async_trait]
impl Memory for AgentScopedMarkdownMemory {
    fn name(&self) -> &str {
        // Identical to MarkdownMemory's name so dashboards and log
        // grep keep working.
        self.own.name()
    }

    async fn health_check(&self) -> bool {
        self.own.health_check().await
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.own.store(key, content, category, session_id).await
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
        self.own
            .store_with_metadata(key, content, category, session_id, namespace, importance)
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
        _agent_id: Option<&str>,
    ) -> Result<()> {
        // Markdown attribution lives on the on-disk path; the bound
        // agent's MarkdownMemory always writes to its own dir, so the
        // caller-supplied agent_id is intentionally ignored here.
        self.own
            .store_with_metadata(key, content, category, session_id, namespace, importance)
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
        let mut merged = Self::attribute(
            &self.own_alias,
            self.own
                .recall(query, limit, session_id, since, until)
                .await?,
        );
        for peer in &self.peers {
            match peer
                .memory
                .recall(query, limit, session_id, since, until)
                .await
            {
                Ok(rows) => merged.extend(Self::attribute(&peer.alias, rows)),
                Err(error) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"peer": peer.alias, "error": format!("{}", error)})
                        ),
                    "AgentScopedMarkdownMemory peer recall failed; continuing with other peers"
                ),
            }
        }
        merged.truncate(limit);
        Ok(merged)
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Empty allowlist means "no extra restriction" — fall back to
        // the bound own + all peers union.
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }

        // The trait passes UUID strings; for Markdown the runtime
        // factory passes alias strings (Markdown has no UUID indirection
        // at the storage layer). We treat the strings as opaque
        // identifiers and intersect with own_alias + peer aliases.
        let mut merged = Vec::new();
        if allowed_agent_ids.contains(&self.own_alias.as_str()) {
            merged.extend(Self::attribute(
                &self.own_alias,
                self.own
                    .recall(query, limit, session_id, since, until)
                    .await?,
            ));
        }
        for peer in &self.peers {
            if !allowed_agent_ids.contains(&peer.alias.as_str()) {
                continue;
            }
            match peer
                .memory
                .recall(query, limit, session_id, since, until)
                .await
            {
                Ok(rows) => merged.extend(Self::attribute(&peer.alias, rows)),
                Err(error) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"peer": peer.alias, "error": format!("{}", error)})
                        ),
                    "AgentScopedMarkdownMemory peer recall failed; continuing with other peers"
                ),
            }
        }
        merged.truncate(limit);
        Ok(merged)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let entry = self.own.get(key).await?;
        Ok(entry.map(|mut e| {
            e.agent_alias = Some(self.own_alias.clone());
            e.agent_id = Some(self.own_alias.clone());
            e
        }))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let entries = self.own.list(category, session_id).await?;
        Ok(Self::stamp_attribution(&self.own_alias, entries))
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        self.own.forget(key).await
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> Result<bool> {
        self.own.forget_for_agent(key, agent_id).await
    }

    async fn count(&self) -> Result<usize> {
        self.own.count().await
    }
}

impl ::zeroclaw_api::attribution::Attributable for AgentScopedMarkdownMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(
            ::zeroclaw_api::attribution::MemoryKind::AgentScopedMarkdown,
        )
    }
    fn alias(&self) -> &str {
        &self.own_alias
    }
}

#[cfg(test)]
mod tests {
    use super::super::markdown::MarkdownMemory;
    use super::*;
    use tempfile::TempDir;

    fn make_md(name: &str) -> (TempDir, MarkdownMemory) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mem = MarkdownMemory::new("markdown", &dir);
        (tmp, mem)
    }

    #[tokio::test]
    async fn store_writes_only_to_own_backend() {
        let (_tmp_a, own) = make_md("alpha-ws");
        let (_tmp_b, peer_mem) = make_md("beta-ws");
        let scoped = AgentScopedMarkdownMemory::new(
            "alpha",
            Arc::new(own),
            vec![MarkdownPeer {
                alias: "beta".into(),
                memory: Arc::new(peer_mem),
            }],
        );

        scoped
            .store("k1", "alpha-only", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Recall returns only the alpha-attributed row; beta's
        // workspace was never written.
        let hits = scoped
            .recall("alpha-only", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(
            hits[0].key.starts_with("[alpha] "),
            "own-backend rows must surface with [alpha] attribution"
        );
    }

    #[tokio::test]
    async fn recall_unions_own_and_peer_rows_with_attribution() {
        let (_tmp_a, own) = make_md("alpha-ws");
        let (_tmp_b, peer_mem) = make_md("beta-ws");

        // Seed the peer's MarkdownMemory directly so the recall has
        // something on the peer side to merge.
        peer_mem
            .store("shared", "beta-content", MemoryCategory::Core, None)
            .await
            .unwrap();

        let scoped = AgentScopedMarkdownMemory::new(
            "alpha",
            Arc::new(own),
            vec![MarkdownPeer {
                alias: "beta".into(),
                memory: Arc::new(peer_mem),
            }],
        );

        // Now seed the own side too.
        scoped
            .store("shared", "alpha-content", MemoryCategory::Core, None)
            .await
            .unwrap();

        let hits = scoped.recall("shared", 10, None, None, None).await.unwrap();
        let attribution_set: std::collections::HashSet<&str> =
            hits.iter().map(|h| h.key.as_str()).collect();
        assert!(
            attribution_set.iter().any(|k| k.starts_with("[alpha] ")),
            "merged recall must include alpha-attributed rows"
        );
        assert!(
            attribution_set.iter().any(|k| k.starts_with("[beta] ")),
            "merged recall must include beta-attributed rows"
        );
    }

    #[tokio::test]
    async fn recall_for_agents_filters_to_alias_intersection() {
        let (_tmp_a, own) = make_md("alpha-ws");
        let (_tmp_b, peer_mem) = make_md("beta-ws");

        peer_mem
            .store("peer-only", "beta-content", MemoryCategory::Core, None)
            .await
            .unwrap();

        let scoped = AgentScopedMarkdownMemory::new(
            "alpha",
            Arc::new(own),
            vec![MarkdownPeer {
                alias: "beta".into(),
                memory: Arc::new(peer_mem),
            }],
        );

        // Caller asks ONLY for alpha — beta rows must not surface.
        let hits = scoped
            .recall_for_agents(&["alpha"], "peer-only", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            !hits.iter().any(|h| h.key.starts_with("[beta] ")),
            "caller-restricted recall must drop unlisted peer rows"
        );

        // Caller asks ONLY for beta — alpha (own) rows must not surface.
        let hits = scoped
            .recall_for_agents(&["beta"], "peer-only", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            !hits.iter().any(|h| h.key.starts_with("[alpha] ")),
            "caller-restricted recall must drop own rows when own is not on the caller list"
        );
        assert!(
            hits.iter().any(|h| h.key.starts_with("[beta] ")),
            "caller-restricted recall must include the requested peer's rows"
        );
    }

    // Dashboard parity with SQL backends: every row surfaced through the
    // Markdown wrapper must carry `agent_alias` (and `agent_id`) so the
    // /api/memory response renders the agent chip correctly, the same as
    // SQL JOIN-resolved rows.
    #[tokio::test]
    async fn list_and_get_stamp_agent_alias_for_dashboard_parity() {
        let (_tmp, own) = make_md("alpha-ws");
        let scoped = AgentScopedMarkdownMemory::new("alpha", Arc::new(own), vec![]);

        scoped
            .store("note", "preferences", MemoryCategory::Core, None)
            .await
            .unwrap();

        let list_rows = scoped.list(None, None).await.unwrap();
        assert!(!list_rows.is_empty(), "list must return the stored row");
        for row in &list_rows {
            assert_eq!(
                row.agent_alias.as_deref(),
                Some("alpha"),
                "list rows must be stamped with the bound agent's alias"
            );
            assert_eq!(
                row.agent_id.as_deref(),
                Some("alpha"),
                "agent_id mirrors agent_alias on Markdown (no UUID indirection)"
            );
        }

        let key = &list_rows[0].key;
        let got = scoped
            .get(key)
            .await
            .unwrap()
            .expect("get must find the row");
        assert_eq!(got.agent_alias.as_deref(), Some("alpha"));
        assert_eq!(got.agent_id.as_deref(), Some("alpha"));
    }

    // The recall path's `[alpha] ` key-prefix attribution must coexist
    // with the new field-level attribution. The fields are what the
    // dashboard reads; the prefix is what prompts / logs read.
    #[tokio::test]
    async fn recall_attribution_carries_through_both_key_prefix_and_alias_field() {
        let (_tmp_a, own) = make_md("alpha-ws");
        let (_tmp_b, peer_mem) = make_md("beta-ws");
        peer_mem
            .store("peer-note", "from beta", MemoryCategory::Core, None)
            .await
            .unwrap();
        let scoped = AgentScopedMarkdownMemory::new(
            "alpha",
            Arc::new(own),
            vec![MarkdownPeer {
                alias: "beta".into(),
                memory: Arc::new(peer_mem),
            }],
        );
        scoped
            .store("own-note", "from alpha", MemoryCategory::Core, None)
            .await
            .unwrap();

        let hits = scoped.recall("from", 10, None, None, None).await.unwrap();
        let alpha_hit = hits.iter().find(|h| h.key.starts_with("[alpha] ")).unwrap();
        let beta_hit = hits.iter().find(|h| h.key.starts_with("[beta] ")).unwrap();
        assert_eq!(alpha_hit.agent_alias.as_deref(), Some("alpha"));
        assert_eq!(beta_hit.agent_alias.as_deref(), Some("beta"));
    }
}
