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
    /// Memory kind, orthogonal to the durability/recency category.
    #[serde(default)]
    pub kind: Option<MemoryKind>,
    /// Whether this entry is protected from budget eviction.
    #[serde(default)]
    pub pinned: bool,
    /// Tenant or end-user scope for multi-user memory isolation.
    #[serde(default)]
    pub tenant_id: Option<String>,
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
            .field("kind", &self.kind)
            .field("pinned", &self.pinned)
            .field("tenant_id", &self.tenant_id)
            .field("agent_alias", &self.agent_alias)
            .finish_non_exhaustive()
    }
}

/// Memory kind, orthogonal to [`MemoryCategory`].
/// Epic A owns this shared type and storage field. Later epics classify writes
/// into kinds and use them during recall and context assembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// Session or event memory.
    Episodic,
    /// Evergreen semantic memory.
    Semantic(SemanticSubtype),
    /// How-to or process memory.
    Procedural,
}

/// Semantic memory subtypes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSubtype {
    Preference,
    Fact,
    Decision,
    Entity,
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

/// A single message in a conversation trace for procedural memory.
/// Used to capture "how to" patterns from tool-calling turns so that
/// backends that support procedural storage can learn from them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProceduralMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Options for storing memory metadata without growing write-method arity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreOptions {
    pub namespace: Option<String>,
    pub importance: Option<f64>,
    pub kind: Option<MemoryKind>,
    pub pinned: bool,
    pub tenant_id: Option<String>,
}

impl StoreOptions {
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }

    pub fn with_importance(mut self, importance: f64) -> Self {
        self.importance = Some(importance);
        self
    }

    pub fn with_kind(mut self, kind: MemoryKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }
}

/// Read-side memory store telemetry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_rows: u64,
    pub by_category: Vec<(String, u64)>,
    pub superseded_rows: u64,
    pub pinned_rows: u64,
    pub bytes: u64,
}

/// Shared memory policy decision substrate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum MemoryPolicyDecision {
    Allow,
    Deny { reason: String },
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

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;

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

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool>;

    /// Remove all memories whose `namespace` field equals the given value.
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

    async fn purge_session_for_agent(
        &self,
        _session_id: &str,
        _agent_id: &str,
    ) -> anyhow::Result<usize> {
        anyhow::bail!("purge_session_for_agent not supported by this memory backend")
    }

    async fn purge_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
        anyhow::bail!("purge_agent not supported by this memory backend")
    }

    /// Export every memory row attributed to `agent_alias`, for the agent-
    /// deletion archive (export-then-delete, Pairs with
    /// [`Self::purge_agent`]: the surface exports these rows to the archive,
    /// then purges. Default: empty (backends without per-agent export).
    async fn export_agent(&self, _agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn rename_agent(&self, _from: &str, _to: &str) -> anyhow::Result<usize> {
        anyhow::bail!("rename_agent not supported by this memory backend")
    }

    async fn count_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
        Ok(0)
    }

    /// Count total memories
    async fn count(&self) -> anyhow::Result<usize>;

    /// Health check
    async fn health_check(&self) -> bool;

    /// Mark entries as superseded by a newer row.
    /// Default: no-op. SQL backends can override this with reversible
    /// soft-hide behavior; non-SQL backends remain source-compatible.
    async fn supersede(&self, _superseded_ids: &[String], _new_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Store a procedural "how to" trace from a tool-calling turn.
    /// Default: no-op. Backends that support procedural storage can override.
    async fn store_procedural(
        &self,
        _messages: &[ProceduralMessage],
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Count rows within a namespace/category scope.
    /// Default is zero so quota enforcement remains opt-in until a backend
    /// provides an efficient implementation.
    async fn count_in_scope(
        &self,
        _namespace: Option<&str>,
        _category: Option<&MemoryCategory>,
    ) -> anyhow::Result<u64> {
        Ok(0)
    }

    /// Read-side memory store telemetry.
    /// Default is empty telemetry so status consumers can be introduced before
    /// every backend has native stats support.
    async fn stats(&self) -> anyhow::Result<MemoryStats> {
        Ok(MemoryStats::default())
    }

    async fn reindex(&self) -> anyhow::Result<usize> {
        Ok(0)
    }

    fn refresh_embedder(
        &self,
        _model_provider: &str,
        _api_key: Option<&str>,
        _model: &str,
        _dimensions: usize,
    ) {
    }

    /// Recall memories scoped to a specific namespace.
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

    /// Store a memory entry with the full additive metadata surface.
    /// Default delegates through the existing metadata method and intentionally
    /// ignores fields that older backends do not yet persist.
    async fn store_with_options(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
    ) -> anyhow::Result<()> {
        self.store_with_metadata(
            key,
            content,
            category,
            session_id,
            options.namespace.as_deref(),
            options.importance,
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
    ) -> anyhow::Result<()>;

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>>;

    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        Ok(alias.to_string())
    }
}

/// High-level memory lifecycle policy.
/// Implemented by strategy objects that wrap one or more `Memory` backends.
#[async_trait]
pub trait MemoryStrategy: Send + Sync {
    /// Consolidate a conversation turn into long-term memory.
    async fn consolidate_turn(
        &self,
        user_message: &str,
        assistant_response: &str,
        provider: &dyn crate::model_provider::ModelProvider,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<()>;

    /// Run memory governance (cleanup, archiving, background consolidation).
    async fn run_governance(&self) -> anyhow::Result<()>;
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
            kind: None,
            pinned: false,
            tenant_id: None,
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
        assert!(parsed.kind.is_none());
        assert!(!parsed.pinned);
        assert!(parsed.tenant_id.is_none());
    }

    #[test]
    fn memory_entry_defaults_new_memory_plane_fields_when_absent() {
        let json = r#"{
            "id": "id-1",
            "key": "favorite_language",
            "content": "Rust",
            "category": "core",
            "timestamp": "2026-02-16T00:00:00Z",
            "session_id": null,
            "score": null
        }"#;

        let parsed: MemoryEntry = serde_json::from_str(json).unwrap();

        assert!(parsed.kind.is_none());
        assert!(!parsed.pinned);
        assert!(parsed.tenant_id.is_none());
    }

    #[test]
    fn memory_entry_roundtrip_preserves_new_memory_plane_fields() {
        let entry = MemoryEntry {
            id: "id-2".into(),
            key: "deployment_decision".into(),
            content: "Use staged rollout".into(),
            category: MemoryCategory::Core,
            timestamp: "2026-02-16T00:00:00Z".into(),
            session_id: None,
            score: None,
            namespace: "ops".into(),
            importance: Some(0.9),
            superseded_by: None,
            kind: Some(MemoryKind::Semantic(SemanticSubtype::Decision)),
            pinned: true,
            tenant_id: Some("tenant-1".into()),
            agent_alias: Some("agent-a".into()),
            agent_id: Some("agent-uuid".into()),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: MemoryEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(
            parsed.kind,
            Some(MemoryKind::Semantic(SemanticSubtype::Decision))
        );
        assert!(parsed.pinned);
        assert_eq!(parsed.tenant_id.as_deref(), Some("tenant-1"));
    }
}
