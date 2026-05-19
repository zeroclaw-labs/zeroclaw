use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Filter criteria for bulk memory export (GDPR Art. 20 data portability).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportFilter {
    pub namespace: Option<String>,
    pub session_id: Option<String>,
    pub category: Option<MemoryCategory>,
    /// RFC 3339 lower bound (inclusive) on created_at.
    pub since: Option<String>,
    /// RFC 3339 upper bound (inclusive) on created_at.
    pub until: Option<String>,
}

/// A single message in a conversation trace for procedural memory.
///
/// Used to capture "how to" patterns from tool-calling turns so that
/// backends that support procedural storage can learn from them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProceduralMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A single memory entry
#[derive(Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub key: String,
    pub content: String,
    pub category: MemoryCategory,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub score: Option<f64>,
    /// Namespace for isolation between agents/contexts.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Importance score (0.0–1.0) for prioritized retrieval.
    #[serde(default)]
    pub importance: Option<f64>,
    /// If this entry was superseded by a newer conflicting entry.
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// Resolved, human-readable agent alias for this row (the HashMap key
    /// in `Config::agents`, e.g. `"clamps"`). SQL-backed stores produce
    /// this via `LEFT JOIN agents ON agents.id = memories.agent_id`;
    /// Markdown / Qdrant / None backends populate it with the raw column
    /// value (which is itself the alias for those backends).
    ///
    /// Use this field for display / routing. For scope-equality checks
    /// (e.g. inside `AgentScopedMemory`) use [`MemoryEntry::agent_id`]
    /// instead since that's stable across backend kinds (UUID for SQL,
    /// alias for non-SQL).
    #[serde(default)]
    pub agent_alias: Option<String>,
    /// Raw value of the storage layer's agent column. For SQL backends
    /// this is the `memories.agent_id` UUID FK to `agents.id`; for
    /// Markdown / Qdrant / None this is the alias string. The scoping
    /// wrapper compares on this field so backend-kind doesn't matter.
    #[serde(default, alias = "agent_id")]
    pub agent_id: Option<String>,
}

fn default_namespace() -> String {
    "default".into()
}

impl std::fmt::Debug for MemoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEntry")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("content", &self.content)
            .field("category", &self.category)
            .field("timestamp", &self.timestamp)
            .field("score", &self.score)
            .field("namespace", &self.namespace)
            .field("importance", &self.importance)
            .field("agent_alias", &self.agent_alias)
            .finish_non_exhaustive()
    }
}

/// Memory categories for organization
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCategory {
    /// Long-term facts, preferences, decisions
    Core,
    /// Daily session logs
    Daily,
    /// Conversation context
    Conversation,
    /// User-defined custom category
    Custom(String),
}

impl serde::Serialize for MemoryCategory {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for MemoryCategory {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "core" => Self::Core,
            "daily" => Self::Daily,
            "conversation" => Self::Conversation,
            _ => Self::Custom(s),
        })
    }
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Daily => write!(f, "daily"),
            Self::Conversation => write!(f, "conversation"),
            Self::Custom(name) => write!(f, "{name}"),
        }
    }
}

/// Returns true when a recall query should be interpreted as recent/time-only recall.
///
/// A bare "*" is intentionally equivalent to an omitted query for tool-call
/// compatibility. Non-bare wildcard terms such as "wild*" remain keyword queries.
pub fn is_recent_recall_query(query: &str) -> bool {
    let trimmed = query.trim();
    trimmed.is_empty() || trimmed == "*"
}

/// Normalizes recent/time-only recall queries to the backend-neutral empty query.
pub fn normalize_recent_recall_query(query: &str) -> &str {
    if is_recent_recall_query(query) {
        ""
    } else {
        query
    }
}

/// Core memory trait — implement for any persistence backend
#[async_trait]
pub trait Memory: Send + Sync + crate::attribution::Attributable {
    /// Backend name
    fn name(&self) -> &str;

    /// Store a memory entry, optionally scoped to a session
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Recall memories matching a query (keyword search), optionally scoped to a session
    /// and time range. Empty, whitespace-only, and bare "*" queries return recent/time-only
    /// entries. Non-bare wildcard terms such as "wild*" remain keyword queries.
    /// Time bounds use RFC 3339 / ISO 8601 format
    /// (e.g. "2025-03-01T00:00:00Z"); inclusive (created_at >= since, created_at <= until).
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Get a specific memory by key.
    ///
    /// After composite uniqueness landed, multiple rows may share a `key`
    /// (one per agent). This method returns *some* matching row without an
    /// agent filter; callers that need an agent-scoped lookup use
    /// [`get_for_agent`](Self::get_for_agent).
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;

    /// Get the memory row matching `(key, agent_id)`. Siblings of the same
    /// key under other agents are invisible.
    ///
    /// The default implementation composes [`get`](Self::get) with an
    /// `agent_id` filter and is only correct for backends whose storage
    /// layout cannot hold more than one row per `key` (markdown's
    /// per-agent dir scheme, the `none` stub). Backends that can hold
    /// multiple rows per `key` (SQL with composite unique, Qdrant)
    /// override this with a native composite lookup.
    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        let hit = self.get(key).await?;
        Ok(hit.filter(|e| e.agent_id.as_deref() == Some(agent_id)))
    }

    /// List all memory keys, optionally filtered by category and/or session
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Remove a memory by key. Deletes every row matching `key`, regardless
    /// of agent attribution. Agent-scoped callers (the `AgentScopedMemory`
    /// wrapper) use [`forget_for_agent`](Self::forget_for_agent) instead.
    async fn forget(&self, key: &str) -> anyhow::Result<bool>;

    /// Remove the row matching `(key, agent_id)`. Siblings of the same key
    /// under other agents are untouched. Returns `true` if a row was
    /// removed. Required: no safe default exists for backends or wrappers
    /// that can hold more than one row per `key` — the unscoped `forget`
    /// would destroy sibling rows.
    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool>;

    /// Remove all memories in a namespace (category).
    /// Returns the number of deleted entries.
    /// Default: returns unsupported error. Backends that support bulk deletion override this.
    async fn purge_namespace(&self, _namespace: &str) -> anyhow::Result<usize> {
        anyhow::bail!("purge_namespace not supported by this memory backend")
    }

    /// Remove all memories in a session.
    /// Returns the number of deleted entries.
    /// Default: returns unsupported error. Backends that support bulk deletion override this.
    async fn purge_session(&self, _session_id: &str) -> anyhow::Result<usize> {
        anyhow::bail!("purge_session not supported by this memory backend")
    }

    /// Remove every memory row attributed to the given agent alias.
    /// Returns the number of deleted entries. Called when an agent alias is
    /// removed from `[agents.<alias>]` so the database doesn't accumulate
    /// rows for retired aliases.
    /// Default: returns unsupported error. Backends with per-agent storage
    /// (sqlite, postgres) override this; backends without (markdown, none)
    /// keep the default and the caller logs a warning.
    async fn purge_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
        anyhow::bail!("purge_agent not supported by this memory backend")
    }

    /// Count total memories
    async fn count(&self) -> anyhow::Result<usize>;

    /// Health check
    async fn health_check(&self) -> bool;

    /// Rebuild backend indexes: FTS tables and any missing embedding vectors.
    ///
    /// Intended as a manual fixup after bulk writes that didn't go through
    /// the normal `store()` path (e.g. `zeroclaw migrate openclaw`, which
    /// uses `NoopEmbedding` for speed and leaves `embedding = NULL` behind).
    /// Returns the number of entries that were re-embedded; backends
    /// without a vector index or with nothing to fill in return 0.
    ///
    /// Default: no-op. Overridden by backends that maintain separate
    /// derived indexes (e.g. `SqliteMemory`).
    async fn reindex(&self) -> anyhow::Result<usize> {
        Ok(0)
    }

    /// Store a conversation trace as procedural memory.
    ///
    /// Backends that support procedural storage override this
    /// to extract "how to" patterns from tool-calling turns.  The default
    /// implementation is a no-op.
    async fn store_procedural(
        &self,
        _messages: &[ProceduralMessage],
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Recall memories scoped to a specific namespace.
    ///
    /// Default implementation delegates to `recall()` and filters by namespace.
    /// Backends with native namespace support should override for efficiency.
    async fn recall_namespaced(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self
            .recall(query, limit * 2, session_id, since, until)
            .await?;
        let filtered: Vec<MemoryEntry> = entries
            .into_iter()
            .filter(|e| e.namespace == namespace)
            .take(limit)
            .collect();
        Ok(filtered)
    }

    /// Bulk-export memories matching the given filter criteria.
    ///
    /// Intended for GDPR Art. 20 data portability. Returns entries ordered by
    /// creation time (ascending). Embeddings are excluded.
    ///
    /// Default implementation delegates to `list()` and post-filters on
    /// namespace and time range. Backends with native query support should
    /// override for efficiency.
    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self
            .list(filter.category.as_ref(), filter.session_id.as_deref())
            .await?;
        let filtered: Vec<MemoryEntry> = entries
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
            .collect();
        Ok(filtered)
    }

    /// Store a memory entry with namespace and importance.
    ///
    /// Default implementation delegates to `store()`. Backends with native
    /// namespace/importance support should override.
    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
    ) -> anyhow::Result<()> {
        self.store(key, content, category, session_id).await
    }

    /// Store a memory entry attributed to an explicit agent UUID.
    /// Every backend must implement this explicitly so the agent_id
    /// is never silently dropped at storage time. Backends with
    /// native agent_id columns (SqliteMemory, PostgresMemory,
    /// LucidMemory) persist the attribution in SQL; MarkdownMemory
    /// attributes via the per-agent directory path; QdrantMemory
    /// persists in the vector payload; NoneMemory is a no-op stub.
    /// `AgentScopedMemory` is the canonical caller.
    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()>;

    /// Recall memory entries scoped to a specific set of agent UUIDs.
    /// When `allowed_agent_ids` is non-empty, the backend filters its
    /// result set to rows whose `agent_id` matches one of the listed
    /// UUIDs (or is NULL, for legacy rows written before the agent_id
    /// column existed). Every backend must implement this explicitly
    /// so the allowlist is never silently dropped at read time.
    ///
    /// For SQL-backed stores the filter is `WHERE agent_id IN (...)`.
    /// For Markdown the implementation walks the allowed agents'
    /// per-agent directories. For Qdrant it's a payload filter on
    /// the `agent_id` field. For None it returns an empty list.
    /// `AgentScopedMemory` is the canonical caller; direct invocation
    /// is also valid for read-only cross-agent queries that bypass
    /// the wrapper.
    ///
    /// Cross-backend allowlist entries are rejected at config load
    /// (`agents.<alias>.workspace.read_memory_from` cannot point at a
    /// sibling on a different memory backend); backends therefore
    /// never need to handle a cross-backend recall.
    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    /// Look up (or create) the identifier the backend uses to refer
    /// to the agent named by `alias`.
    ///
    /// Backends with an `agents` table (SqliteMemory, PostgresMemory,
    /// LucidMemory) return the row's UUID, inserting if absent.
    /// Backends without (MarkdownMemory, QdrantMemory, NoneMemory)
    /// return the alias verbatim — there is no UUID indirection at
    /// the storage layer, so the alias serves as the agent_id.
    /// Default impl returns the alias unchanged; SQL backends
    /// override to do the real lookup.
    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        Ok(alias.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_category_display_outputs_expected_values() {
        assert_eq!(MemoryCategory::Core.to_string(), "core");
        assert_eq!(MemoryCategory::Daily.to_string(), "daily");
        assert_eq!(MemoryCategory::Conversation.to_string(), "conversation");
        assert_eq!(
            MemoryCategory::Custom("project_notes".into()).to_string(),
            "project_notes"
        );
    }

    #[test]
    fn memory_category_serde_uses_snake_case() {
        let core = serde_json::to_string(&MemoryCategory::Core).unwrap();
        let daily = serde_json::to_string(&MemoryCategory::Daily).unwrap();
        let conversation = serde_json::to_string(&MemoryCategory::Conversation).unwrap();

        assert_eq!(core, "\"core\"");
        assert_eq!(daily, "\"daily\"");
        assert_eq!(conversation, "\"conversation\"");
    }

    #[test]
    fn memory_category_custom_roundtrip() {
        let custom = MemoryCategory::Custom("project_notes".into());
        let json = serde_json::to_string(&custom).unwrap();
        assert_eq!(json, "\"project_notes\"");
        let parsed: MemoryCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, custom);
    }

    #[test]
    fn memory_entry_roundtrip_preserves_optional_fields() {
        let entry = MemoryEntry {
            id: "id-1".into(),
            key: "favorite_language".into(),
            content: "Rust".into(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-16T00:00:00Z".into(),
            session_id: Some("session-abc".into()),
            score: Some(0.98),
            namespace: "default".into(),
            importance: Some(0.7),
            superseded_by: None,
            agent_alias: None,
            agent_id: None,
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "id-1");
        assert_eq!(parsed.key, "favorite_language");
        assert_eq!(parsed.content, "Rust");
        assert_eq!(parsed.category, MemoryCategory::Core);
        assert_eq!(parsed.session_id.as_deref(), Some("session-abc"));
        assert_eq!(parsed.score, Some(0.98));
        assert_eq!(parsed.namespace, "default");
        assert_eq!(parsed.importance, Some(0.7));
        assert!(parsed.superseded_by.is_none());
    }
}
