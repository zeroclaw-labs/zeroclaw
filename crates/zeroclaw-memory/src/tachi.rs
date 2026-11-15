//! ZeroClaw memory adapter over the shared portable kernel (feature `tachi`).
//!
//! # Licensing
//!
//! `portable-kernel` / `memcore` is licensed under **AGPL-3.0**. Any distributed
//! binary built with the `memory-tachi` / `tachi` feature is therefore
//! AGPL-affected. Keep that feature off for releases that must remain under
//! ZeroClaw's own license terms alone.
//!
//! # Store ownership
//!
//! This adapter owns ZeroClaw's generic memory database. Hypermem is the
//! Hyperion adapter over the same kernel; each host keeps its own behavior
//! and databases. Companion memory uses a separate role and file, while Soul
//! and User Model retain their existing stores and authority.
//!
//! # Known limitations
//!
//! - `reindex` and `supersede` are not implemented (trait defaults).
//! - Embedding-identity reconciliation (`auto_reindex_on_identity_change`) is
//!   sqlite-specific and not mirrored here.
//! - Snapshot / auto-hydrate are not wired for this backend.
//! - Light-sleep governance (near-dup / promote / stale archive) runs on the
//!   live store handle via [`Memory::run_light_sleep_governance`], invoked from
//!   the factory hygiene cadence in [`crate::create_memory_with_storage_and_routes`]
//!   (and from `DefaultMemoryStrategy::run_governance`).
//! - Optional LLM enrichment (summary/keywords/entities) runs via
//!   [`Memory::run_llm_enrichment`] with a per-call provider from
//!   `DefaultMemoryStrategy::consolidate_turn` (12h state-file cadence).

use super::embeddings::EmbeddingProvider;
use super::traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, MemoryStats, StoreOptions,
    is_recent_recall_query,
};
use crate::kernel_store::{KernelStoreKind, MIGRATE_CLI_HINT};
use crate::point_scope::PointScope;
use anyhow::Context;
use async_trait::async_trait;
use chrono::Local;
use parking_lot::{Mutex, RwLock};
use portable_kernel::{DbOpenContext, HybridWeights, MemoryStore, SearchOptions};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

mod recent;

const METADATA_ZC_KEY: &str = "zeroclaw_key";
pub(crate) const METADATA_ZC_CATEGORY: &str = "zeroclaw_category";
// pub(crate): tachi_governance partitions near-dup on these same keys; a
// rename drifting between the two files would silently re-enable
// cross-agent/namespace merges.
pub(crate) const METADATA_ZC_NAMESPACE: &str = "zeroclaw_namespace";
pub(crate) const METADATA_ZC_AGENT: &str = "zeroclaw_agent";
const METADATA_SESSION_ID: &str = "session_id";
const METADATA_TENANT_ID: &str = "tenant_id";
const METADATA_PINNED: &str = "pinned";
const METADATA_KIND: &str = "kind";
const DEFAULT_AGENT: &str = "default";
const DEFAULT_NAMESPACE: &str = "default";
/// Initial page size when growing a prefix scan to completeness.
const DEFAULT_LIST_PAGE_SIZE: usize = 512;
const MAX_LIST_GROW: usize = 1_048_576;

/// Global rows use agent/key identity; session-bound Conversation rows use
/// agent/session/key identity. Metadata remains authoritative for legacy rows.
/// Point get/forget follows the same host session rules as SQLite.
#[derive(Clone)]
pub struct TachiMemory {
    alias: String,
    db_path: PathBuf,
    store: Arc<Mutex<MemoryStore>>,
    embedder: Arc<RwLock<Arc<dyn EmbeddingProvider>>>,
    vector_weight: f32,
    keyword_weight: f32,
    /// Page size seed for complete prefix scans (overridable in tests).
    list_page_size: usize,
    /// Upper bound for growing prefix scans (overridable in tests).
    max_list_grow: usize,
}

impl TachiMemory {
    pub fn new(alias: &str, workspace_dir: &Path) -> anyhow::Result<Self> {
        Self::with_embedder(
            alias,
            workspace_dir,
            Arc::new(super::embeddings::NoopEmbedding),
            0.7,
            0.3,
        )
    }

    pub fn with_embedder(
        alias: &str,
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
    ) -> anyhow::Result<Self> {
        Self::with_embedder_and_page_size(
            alias,
            workspace_dir,
            embedder,
            vector_weight,
            keyword_weight,
            DEFAULT_LIST_PAGE_SIZE,
            MAX_LIST_GROW,
        )
    }

    /// Like [`Self::with_embedder`], but with injectable list page-size / grow
    /// caps for regression tests.
    pub fn with_embedder_and_page_size(
        alias: &str,
        workspace_dir: &Path,
        embedder: Arc<dyn EmbeddingProvider>,
        vector_weight: f32,
        keyword_weight: f32,
        list_page_size: usize,
        max_list_grow: usize,
    ) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("tachi.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let intent = if db_path.try_exists()? {
            DbOpenContext::open_existing_deny()
        } else {
            DbOpenContext::create_fresh()
        };
        let context = KernelStoreKind::Memory.context(intent);
        let store = MemoryStore::open_with_context(
            db_path
                .to_str()
                .context("tachi memory db path is not valid UTF-8")?,
            &context,
        )
        .map_err(|e| {
            anyhow::Error::msg(format!(
                "failed to open tachi memory db: {e}; runtime opens never migrate or adopt \
                 a store identity. Use `{MIGRATE_CLI_HINT}` for supported legacy stores."
            ))
        })?;
        let list_page_size = list_page_size.max(1);
        Ok(Self {
            alias: alias.to_string(),
            db_path,
            store: Arc::new(Mutex::new(store)),
            embedder: Arc::new(RwLock::new(embedder)),
            vector_weight,
            keyword_weight,
            list_page_size,
            max_list_grow: max_list_grow.max(list_page_size),
        })
    }

    /// Shared memcore handle — governance and tests must reuse this, not reopen.
    pub(crate) fn store_handle(&self) -> &Arc<Mutex<MemoryStore>> {
        &self.store
    }

    /// Run light-sleep governance and return counters (tests / observability).
    pub(crate) fn run_light_sleep_governance_report(
        &self,
    ) -> anyhow::Result<crate::tachi_governance::TachiGovernanceReport> {
        crate::tachi_governance::run_tachi_governance_on_handle(self.store_handle())
    }

    fn hybrid_weights(&self) -> HybridWeights {
        let semantic = f64::from(self.vector_weight);
        let fts = f64::from(self.keyword_weight);
        let remainder = (1.0 - semantic - fts).max(0.0);
        HybridWeights {
            semantic,
            fts,
            symbolic: remainder * 0.5,
            decay: remainder * 0.5,
            use_rrf: true,
        }
    }

    fn category_to_segment(category: &MemoryCategory) -> String {
        match category {
            MemoryCategory::Core => "core".into(),
            MemoryCategory::Daily => "daily".into(),
            MemoryCategory::Conversation => "conversation".into(),
            MemoryCategory::Custom(name) => Self::encode_path_segment(name),
        }
    }

    fn category_to_memcore(category: &MemoryCategory) -> String {
        match category {
            MemoryCategory::Core => "fact".into(),
            MemoryCategory::Daily => "experience".into(),
            MemoryCategory::Conversation => "experience".into(),
            MemoryCategory::Custom(_) => "other".into(),
        }
    }

    fn segment_to_category(_segment: &str, metadata: &serde_json::Value) -> MemoryCategory {
        if let Some(stored) = metadata.get(METADATA_ZC_CATEGORY).and_then(|v| v.as_str()) {
            return match stored {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                other => MemoryCategory::Custom(other.to_string()),
            };
        }
        MemoryCategory::Core
    }

    fn sanitize_path_segment(value: &str) -> String {
        value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Injective path segment: always `{sanitized}__{hash}` where `hash` is the
    /// first 8 **bytes** (16 hex chars, 64 bits) of SHA-256 of the original, so
    /// a literal key that looks like an encoded form cannot collide with the
    /// encoding of a different key. Logical identity lives in metadata.
    fn encode_path_segment(value: &str) -> String {
        let sanitized = Self::sanitize_path_segment(value);
        let hash = Self::short_hash(value);
        if sanitized.is_empty() {
            format!("x__{hash}")
        } else {
            format!("{sanitized}__{hash}")
        }
    }

    fn short_hash(value: &str) -> String {
        let digest = Sha256::digest(value.as_bytes());
        format!(
            "{:016x}",
            u64::from_be_bytes(
                digest[..8]
                    .try_into()
                    .expect("SHA-256 always produces >= 8 bytes")
            )
        )
    }

    fn agent_segment(agent_id: Option<&str>) -> String {
        Self::encode_path_segment(agent_id.unwrap_or(DEFAULT_AGENT))
    }

    fn namespace_segment(namespace: Option<&str>) -> String {
        Self::encode_path_segment(namespace.unwrap_or(DEFAULT_NAMESPACE))
    }

    pub(crate) fn storage_path(
        agent_id: Option<&str>,
        namespace: Option<&str>,
        category: &MemoryCategory,
        key: &str,
    ) -> String {
        format!(
            "/agents/{}/{}/{}/{}",
            Self::agent_segment(agent_id),
            Self::namespace_segment(namespace),
            Self::category_to_segment(category),
            Self::encode_path_segment(key)
        )
    }

    fn scoped_storage_path(
        agent_id: Option<&str>,
        namespace: Option<&str>,
        category: &MemoryCategory,
        key: &str,
        conversation_session: Option<&str>,
    ) -> String {
        match conversation_session {
            Some(session) => format!(
                "/agents/{}/{}/{}/sessions/{}/{}",
                Self::agent_segment(agent_id),
                Self::namespace_segment(namespace),
                Self::category_to_segment(category),
                Self::encode_path_segment(session),
                Self::encode_path_segment(key),
            ),
            None => Self::storage_path(agent_id, namespace, category, key),
        }
    }

    pub(crate) fn conversation_session(metadata: &serde_json::Value) -> Option<&str> {
        if metadata.get(METADATA_ZC_CATEGORY).and_then(|v| v.as_str()) == Some("conversation") {
            metadata.get(METADATA_SESSION_ID).and_then(|v| v.as_str())
        } else {
            None
        }
    }

    fn build_metadata(
        key: &str,
        category: &MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        agent_id: Option<&str>,
        options: &StoreOptions,
    ) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            METADATA_ZC_KEY.to_string(),
            serde_json::Value::String(key.to_string()),
        );
        obj.insert(
            METADATA_ZC_CATEGORY.to_string(),
            serde_json::Value::String(category.to_string()),
        );
        obj.insert(
            METADATA_ZC_NAMESPACE.to_string(),
            serde_json::Value::String(namespace.unwrap_or(DEFAULT_NAMESPACE).to_string()),
        );
        obj.insert(
            METADATA_ZC_AGENT.to_string(),
            serde_json::Value::String(agent_id.unwrap_or(DEFAULT_AGENT).to_string()),
        );
        if let Some(sid) = session_id {
            obj.insert(
                METADATA_SESSION_ID.to_string(),
                serde_json::Value::String(sid.to_string()),
            );
        }
        if let Some(tenant) = options.tenant_id.as_deref() {
            obj.insert(
                METADATA_TENANT_ID.to_string(),
                serde_json::Value::String(tenant.to_string()),
            );
        }
        if options.pinned {
            obj.insert(METADATA_PINNED.to_string(), serde_json::Value::Bool(true));
        }
        if let Some(kind) = &options.kind
            && let Ok(value) = serde_json::to_value(kind)
        {
            obj.insert(METADATA_KIND.to_string(), value);
        }
        serde_json::Value::Object(obj)
    }

    fn metadata_str(metadata: &serde_json::Value, key: &str) -> Option<String> {
        metadata
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    async fn compute_embedding(&self, text: &str) -> Option<Vec<f32>> {
        let embedder = self.embedder.read().clone();
        if embedder.dimensions() == 0 {
            return None;
        }
        embedder.embed_one(text).await.ok()
    }

    fn memcore_to_zeroclaw(entry: portable_kernel::MemoryEntry, score: Option<f64>) -> MemoryEntry {
        let metadata = entry.metadata.clone();
        let key = Self::metadata_str(&metadata, METADATA_ZC_KEY).unwrap_or_else(|| {
            entry
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&entry.path)
                .to_string()
        });
        let category = Self::segment_to_category("", &metadata);
        let session_id = Self::metadata_str(&metadata, METADATA_SESSION_ID);
        let tenant_id = Self::metadata_str(&metadata, METADATA_TENANT_ID);
        let pinned = metadata
            .get(METADATA_PINNED)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let kind = metadata
            .get(METADATA_KIND)
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let agent_id = Self::metadata_str(&metadata, METADATA_ZC_AGENT);
        let namespace = Self::metadata_str(&metadata, METADATA_ZC_NAMESPACE)
            .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string());
        MemoryEntry {
            id: entry.id,
            key,
            content: entry.text,
            category,
            timestamp: entry.timestamp,
            session_id,
            score,
            namespace,
            importance: Some(entry.importance),
            superseded_by: None,
            kind,
            pinned,
            tenant_id,
            agent_alias: agent_id.clone(),
            agent_id,
        }
    }

    fn entry_in_window(timestamp: &str, since: Option<&str>, until: Option<&str>) -> bool {
        if let Some(s) = since
            && timestamp < s
        {
            return false;
        }
        if let Some(u) = until
            && timestamp > u
        {
            return false;
        }
        true
    }

    fn filter_entries(
        entries: Vec<MemoryEntry>,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        namespace: Option<&str>,
        allowed_agents: Option<&[&str]>,
        limit: usize,
    ) -> Vec<MemoryEntry> {
        entries
            .into_iter()
            .filter(|entry| {
                if let Some(sid) = session_id
                    && entry.session_id.as_deref() != Some(sid)
                {
                    return false;
                }
                if !Self::entry_in_window(&entry.timestamp, since, until) {
                    return false;
                }
                if let Some(ns) = namespace
                    && entry.namespace != ns
                {
                    return false;
                }
                // Ambient recall (no namespace scope) structurally excludes
                // the reserved Soul namespace; only `recall_namespaced`
                // opts in.
                if namespace.is_none() && entry.namespace == crate::SOUL_NAMESPACE {
                    return false;
                }
                if let Some(allowed) = allowed_agents {
                    let agent = entry.agent_id.as_deref().unwrap_or(DEFAULT_AGENT);
                    if !allowed.contains(&agent) {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .collect()
    }

    /// Grow `list_by_path` until the page is not full. If the page is still
    /// full at `max_grow`, fail loud — silent truncation would leave rows
    /// surviving purge/delete.
    fn list_prefix_complete(
        store: &MemoryStore,
        prefix: &str,
        page_size: usize,
        max_grow: usize,
    ) -> anyhow::Result<Vec<portable_kernel::MemoryEntry>> {
        let mut limit = page_size.max(1);
        let max_grow = max_grow.max(limit);
        loop {
            let rows = store
                .list_by_path(prefix, limit, false)
                .map_err(|e| anyhow::Error::msg(format!("tachi list failed: {e}")))?;
            if rows.len() < limit {
                return Ok(rows);
            }
            if limit >= max_grow {
                return Err(anyhow::Error::msg(format!(
                    "tachi list truncated: prefix={prefix} returned {limit} rows at max_list_grow={max_grow}"
                )));
            }
            limit = limit.saturating_mul(2).min(max_grow);
        }
    }

    fn agent_path_prefix(agent: Option<&str>) -> String {
        match agent {
            Some(agent) => format!("/agents/{}/", Self::agent_segment(Some(agent))),
            None => "/agents/".into(),
        }
    }

    /// Use the admitted live kernel handle and its exact streamed metadata
    /// candidate lookup (`find_metadata_key_candidates`). No independent SQLite
    /// connection, raw SQL, schema change or lookup cache. Only matching ids
    /// cross the kernel boundary and load text/vector bodies; corrupt
    /// nonmatching metadata within the literal path prefix still fails closed,
    /// and matching rows preserve legacy `path ASC, timestamp DESC` ordering.
    /// This does not promise an indexed/O(1) key lookup.
    fn rows_matching_key(
        store: &MemoryStore,
        key: &str,
        agent: Option<&str>,
    ) -> anyhow::Result<Vec<portable_kernel::MemoryEntry>> {
        // Memcore enforces 1..=4096 for max_matches and fails closed rather than
        // truncating if a single logical key ever exceeds this cap.
        const MAX_KEY_CANDIDATES: usize = 4096;
        let prefix = Self::agent_path_prefix(agent);
        let mut predicates = Vec::with_capacity(2);
        predicates.push((METADATA_ZC_KEY, key));
        if let Some(agent) = agent {
            predicates.push((METADATA_ZC_AGENT, agent));
        }
        let candidates = store
            .find_metadata_key_candidates(&prefix, &predicates, MAX_KEY_CANDIDATES)
            .map_err(|error| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"phase": "key_candidate_lookup"})),
                    "tachi memory key candidate lookup failed"
                );
                anyhow::Error::msg(format!("tachi key candidate lookup failed: {error}"))
            })?;
        let mut result = Vec::with_capacity(candidates.len());
        for row in candidates {
            // The projection includes archived rows. Use the normal typed get
            // to retain this adapter's active-only visibility.
            if let Some(entry) = store.get(&row.id).map_err(|error| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"phase": "key_row_lookup"})),
                    "tachi memory key row lookup failed"
                );
                anyhow::Error::msg(format!("tachi key row lookup failed: {error}"))
            })? {
                result.push(entry);
            }
        }
        // Preserve list_by_path's ordering before PointScope and point reads'
        // pop(), or writes' first matching identity, including legacy rows.
        result.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then_with(|| b.timestamp.cmp(&a.timestamp))
        });
        Ok(result)
    }

    async fn store_internal(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
        options: StoreOptions,
    ) -> anyhow::Result<()> {
        // Same storage-level reservation as sqlite: rows under the reserved
        // Soul key prefix exist only in the reserved Soul namespace, and the
        // reserved namespace accepts only reserved-prefix keys.
        let ns = namespace.unwrap_or("default");
        if ns == crate::SOUL_NAMESPACE {
            if !key.starts_with(crate::SOUL_KEY_PREFIX) {
                anyhow::bail!(
                    "refused: namespace '{}' requires a key with the reserved '{}' prefix",
                    crate::SOUL_NAMESPACE,
                    crate::SOUL_KEY_PREFIX
                );
            }
        } else if key.starts_with(crate::SOUL_KEY_PREFIX) {
            anyhow::bail!(
                "refused: key prefix '{}' is reserved for the Soul namespace",
                crate::SOUL_KEY_PREFIX
            );
        }
        // Only Conversation sessions split the global identity. A Core/Daily
        // row can carry session provenance without becoming a scoped row.
        let conversation_session = if category.to_string() == "conversation" {
            session_id.map(str::to_owned)
        } else {
            None
        };
        let path = Self::scoped_storage_path(
            agent_id,
            namespace,
            &category,
            key,
            conversation_session.as_deref(),
        );
        // Soul persistence does not authorize external embedding of its content.
        let embedding = if ns == crate::SOUL_NAMESPACE {
            None
        } else {
            self.compute_embedding(content).await
        };
        let now = Local::now().to_rfc3339();
        let importance = importance.unwrap_or(0.7);
        let scope = match namespace {
            Some("default") | None => "general".to_string(),
            Some("user") => "user".to_string(),
            Some(ns) if ns.starts_with("user:") => "user".to_string(),
            _ => "project".to_string(),
        };
        let metadata =
            Self::build_metadata(key, &category, session_id, namespace, agent_id, &options);
        let pinned = options.pinned;

        let store = self.store.clone();
        let path_c = path.clone();
        let content_c = content.to_string();
        let category_c = Self::category_to_memcore(&category);
        let embedding_c = embedding.clone();
        let scope_c = scope;
        let key_owned = key.to_string();
        // Match SQLite's logical identity, including a Conversation session.
        // Namespace/category changes on global rows still update in place.
        let agent_owned = agent_id.unwrap_or(DEFAULT_AGENT).to_string();

        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut store = store.lock();
            let same_identity = |entry: &portable_kernel::MemoryEntry| {
                entry.metadata.get(METADATA_ZC_KEY).and_then(|v| v.as_str())
                    == Some(key_owned.as_str())
                    && entry
                        .metadata
                        .get(METADATA_ZC_AGENT)
                        .and_then(|v| v.as_str())
                        == Some(agent_owned.as_str())
                    && Self::conversation_session(&entry.metadata)
                        == conversation_session.as_deref()
            };
            // Old session rows have an unscoped path. Resolve them by metadata
            // and retain their UUID; another session must never adopt them.
            let exact_hit = store
                .list_by_path(&path_c, 8, false)
                .map_err(|error| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"phase": "identity_lookup"})),
                        "tachi memory identity lookup failed"
                    );
                    anyhow::Error::msg(format!("tachi identity lookup failed: {error}"))
                })?
                .into_iter()
                .find(|entry| entry.path == path_c && same_identity(entry));
            let existing = match exact_hit {
                Some(entry) => Some(entry),
                None => {
                    // Preserve the complete legacy ordering on an exact-path
                    // miss. A canonical candidate cannot establish which
                    // same-identity row list_prefix_complete would pick.
                    Self::rows_matching_key(&store, &key_owned, Some(&agent_owned))?
                        .into_iter()
                        .find(same_identity)
                }
            };

            // Preserve governance-owned fields on update so light-sleep keyword
            // merges and access/recall history are not clobbered by a re-store.
            let id = existing
                .as_ref()
                .map(|e| e.id.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let keywords = existing
                .as_ref()
                .map(|e| e.keywords.clone())
                .unwrap_or_default();
            let access_count = existing.as_ref().map(|e| e.access_count).unwrap_or(0);
            let last_access = existing.as_ref().and_then(|e| e.last_access.clone());
            let recall_count = existing.as_ref().map(|e| e.recall_count).unwrap_or(0);
            let query_diversity = existing.as_ref().map(|e| e.query_diversity).unwrap_or(0);
            let tier = existing
                .as_ref()
                .map(|e| e.tier.clone())
                .unwrap_or_else(portable_kernel::types::default_tier);
            let revision = existing.as_ref().map(|e| e.revision).unwrap_or(1);
            // Map zeroclaw pin ↔ memcore retention_policy so stale archival
            // spares pinned rows (`archive_stale_low_value_memories` exempts
            // retention_policy IN ('permanent','pinned','durable')).
            let retention_policy = if pinned {
                Some("pinned".into())
            } else {
                match existing
                    .as_ref()
                    .and_then(|e| e.retention_policy.as_deref())
                {
                    Some("pinned") => None, // cleared on unpin
                    _ => existing.as_ref().and_then(|e| e.retention_policy.clone()),
                }
            };

            let entry = portable_kernel::MemoryEntry {
                id,
                path: path_c,
                summary: content_c.chars().take(100).collect(),
                text: content_c,
                importance,
                timestamp: now.clone(),
                valid_from: now,
                valid_until: None,
                category: category_c,
                topic: String::new(),
                keywords,
                persons: Vec::new(),
                entities: Vec::new(),
                location: String::new(),
                source: "zeroclaw".into(),
                scope: scope_c,
                archived: false,
                access_count,
                scored_count: 0,
                last_use_at: None,
                last_access,
                revision,
                vector: embedding_c,
                retention_policy,
                domain: None,
                metadata,
                recall_count,
                query_diversity,
                tier,
            };
            store
                .upsert(&entry)
                .map_err(|e| anyhow::Error::msg(format!("tachi upsert failed: {e}")))
        })
        .await??;
        Ok(())
    }

    async fn list_all(&self) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.clone();
        let page_size = self.list_page_size;
        let max_grow = self.max_list_grow;
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let store = store.lock();
            let rows = Self::list_prefix_complete(&store, "/agents", page_size, max_grow)?;
            Ok(rows
                .into_iter()
                .map(|row| Self::memcore_to_zeroclaw(row, None))
                .collect())
        })
        .await?
    }

    async fn find_rows_by_key(
        &self,
        key: &str,
        agent_id: Option<&str>,
        scope: PointScope,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.clone();
        let key = key.to_string();
        let agent = agent_id.map(str::to_owned);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let store = store.lock();
            let rows = Self::rows_matching_key(&store, &key, agent.as_deref())?;
            Ok(scope.select(
                rows.into_iter()
                    .map(|row| Self::memcore_to_zeroclaw(row, None))
                    .collect(),
                agent.is_some(),
            ))
        })
        .await?
    }

    async fn recall_internal(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
        namespace: Option<&str>,
        allowed_agents: Option<&[&str]>,
        path_prefix: Option<String>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let prefix = path_prefix.unwrap_or_else(|| "/agents".to_string());
        if is_recent_recall_query(query) {
            let store = self.store.clone();
            // Apply host scope before the limited read; retain output admission.
            let selections = recent::queries(
                prefix,
                limit,
                session_id,
                since,
                until,
                namespace,
                allowed_agents,
            );
            let mut entries =
                tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
                    let store = store.lock();
                    let rows = recent::read(&store, &selections, limit)?;
                    Ok(rows
                        .into_iter()
                        .map(|row| Self::memcore_to_zeroclaw(row, Some(1.0)))
                        .collect())
                })
                .await??;
            entries = Self::filter_entries(
                entries,
                session_id,
                since,
                until,
                namespace,
                allowed_agents,
                limit,
            );
            return Ok(entries);
        }

        let query_vec = self.compute_embedding(query).await;
        let store = self.store.clone();
        let query_c = query.to_string();
        let weights = self.hybrid_weights();
        let path_prefix_c = Some(prefix);

        let mut results =
            tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
                let store = store.lock();
                let vec_available = store.vec_available;
                let opts = SearchOptions {
                    top_k: limit.max(6),
                    candidates_per_channel: limit.max(20),
                    weights,
                    path_prefix: path_prefix_c,
                    query_vec,
                    vec_available,
                    record_access: true,
                    ..Default::default()
                };
                let hits = store
                    .search(&query_c, Some(opts))
                    .map_err(|e| anyhow::Error::msg(format!("tachi search failed: {e}")))?;
                Ok(hits
                    .into_iter()
                    .map(|hit| Self::memcore_to_zeroclaw(hit.entry, Some(hit.score.final_score)))
                    .collect())
            })
            .await??;

        results = Self::filter_entries(
            results,
            session_id,
            since,
            until,
            namespace,
            allowed_agents,
            limit,
        );
        Ok(results)
    }

    fn swap_embedder(&self, embedder: Arc<dyn EmbeddingProvider>) {
        *self.embedder.write() = embedder;
    }

    async fn delete_ids(&self, ids: Vec<String>) -> anyhow::Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let mut store = store.lock();
            let mut deleted = 0usize;
            for id in ids {
                if store
                    .delete(&id)
                    .map_err(|e| anyhow::Error::msg(format!("{e}")))?
                {
                    deleted += 1;
                }
            }
            Ok(deleted)
        })
        .await?
    }

    async fn list_agent_entries(&self, agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        let store = self.store.clone();
        let page_size = self.list_page_size;
        let max_grow = self.max_list_grow;
        let prefix = format!("/agents/{}/", Self::agent_segment(Some(agent_alias)));
        let agent_alias = agent_alias.to_string();
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<MemoryEntry>> {
            let store = store.lock();
            let rows = Self::list_prefix_complete(&store, &prefix, page_size, max_grow)?;
            Ok(rows
                .into_iter()
                .filter(|row| {
                    Self::metadata_str(&row.metadata, METADATA_ZC_AGENT).as_deref()
                        == Some(agent_alias.as_str())
                        || Self::agent_segment(Some(&agent_alias))
                            == row
                                .path
                                .trim_start_matches('/')
                                .split('/')
                                .nth(1)
                                .unwrap_or("")
                })
                .map(|row| Self::memcore_to_zeroclaw(row, None))
                .collect())
        })
        .await?
    }
}

#[async_trait]
impl Memory for TachiMemory {
    fn name(&self) -> &str {
        &self.alias
    }

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        let embedder: Arc<dyn EmbeddingProvider> =
            Arc::from(super::embeddings::create_embedding_provider(
                model_provider,
                api_key,
                model,
                dimensions,
            ));
        self.swap_embedder(embedder);
    }

    fn run_light_sleep_governance(&self) -> anyhow::Result<()> {
        let _report = self.run_light_sleep_governance_report()?;
        Ok(())
    }

    async fn run_llm_enrichment(
        &self,
        provider: &dyn zeroclaw_api::model_provider::ModelProvider,
        model: &str,
    ) -> anyhow::Result<usize> {
        crate::tachi_enrichment::run_enrichment_pass(self.store_handle(), provider, model).await
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.store_with_agent(key, content, category, session_id, None, None, None)
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
        self.recall_internal(query, limit, session_id, since, until, None, None, None)
            .await
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
        self.recall_internal(
            query,
            limit,
            session_id,
            since,
            until,
            Some(namespace),
            None,
            None,
        )
        .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let scope = PointScope::current();
        // Ambient get never returns a row from the reserved Soul namespace.
        Ok(self.find_rows_by_key(key, None, scope).await?.pop())
    }

    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        let scope = PointScope::current();
        Ok(self
            .find_rows_by_key(key, Some(agent_id), scope)
            .await?
            .pop())
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut entries = self.list_all().await?;
        // Ambient listing excludes the reserved Soul namespace.
        entries.retain(|e| e.namespace != crate::SOUL_NAMESPACE);
        if let Some(cat) = category {
            entries.retain(|e| &e.category == cat);
        }
        if let Some(sid) = session_id {
            entries.retain(|e| e.session_id.as_deref() == Some(sid));
        }
        Ok(entries)
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let scope = PointScope::current();
        let rows = self.find_rows_by_key(key, None, scope).await?;
        // The unscoped delete never reaches the reserved Soul namespace;
        // Soul rows are forgotten only through `forget_for_agent` with the
        // admitted identity.
        let ids: Vec<String> = rows.into_iter().map(|e| e.id).collect();
        Ok(self.delete_ids(ids).await? > 0)
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        let scope = PointScope::current();
        let rows = self.find_rows_by_key(key, Some(agent_id), scope).await?;
        let ids: Vec<String> = rows.into_iter().map(|e| e.id).collect();
        Ok(self.delete_ids(ids).await? > 0)
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        let entries = self.list_all().await?;
        let ids: Vec<String> = entries
            .into_iter()
            .filter(|e| e.namespace == namespace)
            .map(|e| e.id)
            .collect();
        self.delete_ids(ids).await
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        let entries = self.list_all().await?;
        let ids: Vec<String> = entries
            .into_iter()
            .filter(|e| e.session_id.as_deref() == Some(session_id))
            .map(|e| e.id)
            .collect();
        self.delete_ids(ids).await
    }

    async fn purge_session_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<usize> {
        let entries = self.list_all().await?;
        let ids: Vec<String> = entries
            .into_iter()
            .filter(|e| {
                e.session_id.as_deref() == Some(session_id)
                    && e.agent_id.as_deref() == Some(agent_id)
            })
            .map(|e| e.id)
            .collect();
        self.delete_ids(ids).await
    }

    async fn export_agent(&self, agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut entries = self.list_agent_entries(agent_alias).await?;
        entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(entries)
    }

    async fn purge_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        let entries = self.list_agent_entries(agent_alias).await?;
        let ids: Vec<String> = entries.into_iter().map(|e| e.id).collect();
        self.delete_ids(ids).await
    }

    async fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        if from == to {
            return Ok(0);
        }
        let store = self.store.clone();
        let page_size = self.list_page_size;
        let max_grow = self.max_list_grow;
        let from = from.to_string();
        let to = to.to_string();
        let from_seg = Self::agent_segment(Some(&from));
        let to_seg = Self::agent_segment(Some(&to));
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let mut store = store.lock();
            let prefix = format!("/agents/{from_seg}/");
            let rows = Self::list_prefix_complete(&store, &prefix, page_size, max_grow)?;
            let mut rewritten = 0usize;
            for mut row in rows {
                let agent_meta = Self::metadata_str(&row.metadata, METADATA_ZC_AGENT);
                if agent_meta.as_deref() != Some(from.as_str()) && !row.path.starts_with(&prefix) {
                    continue;
                }
                let rest = row
                    .path
                    .strip_prefix(&format!("/agents/{from_seg}"))
                    .unwrap_or("");
                row.path = format!("/agents/{to_seg}{rest}");
                if let Some(obj) = row.metadata.as_object_mut() {
                    obj.insert(
                        METADATA_ZC_AGENT.to_string(),
                        serde_json::Value::String(to.clone()),
                    );
                }
                store
                    .upsert(&row)
                    .map_err(|e| anyhow::Error::msg(format!("tachi rename upsert failed: {e}")))?;
                rewritten += 1;
            }
            Ok(rewritten)
        })
        .await?
    }

    async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        Ok(self.list_agent_entries(agent_alias).await?.len())
    }

    async fn count(&self) -> anyhow::Result<usize> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let store = store.lock();
            let stats = store
                .stats(false)
                .map_err(|e| anyhow::Error::msg(format!("{e}")))?;
            Ok(stats.total as usize)
        })
        .await?
    }

    async fn health_check(&self) -> bool {
        if !self.db_path.exists() {
            return false;
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let store = store.lock();
            store.quick_check().unwrap_or(false)
        })
        .await
        .unwrap_or(false)
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
        self.store_with_agent(
            key, content, category, session_id, namespace, importance, None,
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
    ) -> anyhow::Result<()> {
        let namespace = options.namespace.clone();
        self.store_internal(
            key,
            content,
            category,
            session_id,
            namespace.as_deref(),
            options.importance,
            None,
            options,
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
    ) -> anyhow::Result<()> {
        self.store_internal(
            key,
            content,
            category,
            session_id,
            namespace,
            importance,
            agent_id,
            StoreOptions::default(),
        )
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
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }
        if allowed_agent_ids.len() == 1 {
            let prefix = format!(
                "/agents/{}/",
                Self::agent_segment(Some(allowed_agent_ids[0]))
            );
            return self
                .recall_internal(
                    query,
                    limit,
                    session_id,
                    since,
                    until,
                    None,
                    Some(allowed_agent_ids),
                    Some(prefix),
                )
                .await;
        }
        self.recall_internal(
            query,
            limit,
            session_id,
            since,
            until,
            None,
            Some(allowed_agent_ids),
            Some("/agents".into()),
        )
        .await
    }

    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        let mut entries = self
            .list(filter.category.as_ref(), filter.session_id.as_deref())
            .await?;
        entries.retain(|e| {
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
        });
        Ok(entries)
    }

    async fn stats(&self) -> anyhow::Result<MemoryStats> {
        let store = self.store.clone();
        let page_size = self.list_page_size;
        let max_grow = self.max_list_grow;
        tokio::task::spawn_blocking(move || -> anyhow::Result<MemoryStats> {
            let store = store.lock();
            let stats = store
                .stats(false)
                .map_err(|e| anyhow::Error::msg(format!("{e}")))?;
            let by_category: Vec<(String, u64)> = stats.by_category.into_iter().collect();
            let rows = Self::list_prefix_complete(&store, "/agents", page_size, max_grow)?;
            let mut pinned_rows = 0u64;
            let mut superseded_rows = 0u64;
            let mut bytes = 0u64;
            for row in &rows {
                if row
                    .metadata
                    .get(METADATA_PINNED)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    pinned_rows += 1;
                }
                // memcore soft-hides superseded rows from default list; count
                // only what we can see in metadata if present.
                if row
                    .metadata
                    .get("superseded_by")
                    .and_then(|v| v.as_str())
                    .is_some()
                {
                    superseded_rows += 1;
                }
                bytes += row.text.len() as u64;
            }
            Ok(MemoryStats {
                total_rows: stats.total,
                by_category,
                superseded_rows,
                pinned_rows,
                bytes,
            })
        })
        .await?
    }
}

impl ::zeroclaw_api::attribution::Attributable for TachiMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::Plugin)
    }

    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(all(test, feature = "tachi"))]
mod key_lookup_tests;

#[cfg(all(test, feature = "tachi"))]
mod recent_recall_tests;

#[cfg(all(test, feature = "tachi"))]
mod session_scope_tests;

#[cfg(all(test, feature = "tachi"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_tachi() -> (TempDir, TachiMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
        (tmp, mem)
    }

    #[derive(Default)]
    struct RecordingSoulEmbedding {
        inputs: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EmbeddingProvider for RecordingSoulEmbedding {
        fn name(&self) -> &str {
            "recording-soul-boundary"
        }
        fn dimensions(&self) -> usize {
            // The pinned memcore vec0 schema stores 1024-dimensional vectors.
            1024
        }
        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.inputs
                .lock()
                .extend(texts.iter().map(|text| (*text).to_string()));
            Ok(texts.iter().map(|_| vec![0.2; self.dimensions()]).collect())
        }
    }

    #[tokio::test]
    async fn soul_store_persists_locally_without_implicit_provider_calls() {
        let (_tmp, mem) = temp_tachi();
        let embedder = Arc::new(RecordingSoulEmbedding::default());
        mem.swap_embedder(embedder.clone());
        for (key, namespace) in [
            (
                "ordinary-key".to_string(),
                Some(crate::SOUL_NAMESPACE.to_string()),
            ),
            (format!("{}agent::invalid", crate::SOUL_KEY_PREFIX), None),
        ] {
            mem.store_with_options(
                &key,
                "rejected payload",
                MemoryCategory::Core,
                None,
                StoreOptions {
                    namespace,
                    ..StoreOptions::default()
                },
            )
            .await
            .expect_err("invalid reservation");
            assert!(embedder.inputs.lock().is_empty());
        }
        for suffix in ["disposition", "candidate::pending"] {
            mem.store_with_options(
                &format!("{}agent::{suffix}", crate::SOUL_KEY_PREFIX),
                "reserved local payload",
                MemoryCategory::Core,
                None,
                StoreOptions {
                    namespace: Some(crate::SOUL_NAMESPACE.into()),
                    ..StoreOptions::default()
                },
            )
            .await
            .unwrap();
            assert!(
                embedder.inputs.lock().is_empty(),
                "Soul storage must not send content to the embedder"
            );
        }
        for (key, namespace) in [
            ("ordinary-missing", None),
            ("ordinary-default", Some("default".into())),
        ] {
            mem.store_with_options(
                key,
                key,
                MemoryCategory::Core,
                None,
                StoreOptions {
                    namespace,
                    ..StoreOptions::default()
                },
            )
            .await
            .unwrap();
        }
        assert_eq!(
            &*embedder.inputs.lock(),
            &[
                "ordinary-missing".to_string(),
                "ordinary-default".to_string()
            ]
        );
        // Query-time embedding is a separate surface; use the existing local
        // query path without a provider to inspect persistence after the call audit.
        mem.swap_embedder(Arc::new(super::super::embeddings::NoopEmbedding));
        let rows = mem
            .recall_namespaced(crate::SOUL_NAMESPACE, "*", 10, None, None, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.content == "reserved local payload")
        );
    }

    fn temp_tachi_small_page() -> (TempDir, TachiMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = TachiMemory::with_embedder_and_page_size(
            "tachi",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            2,
            MAX_LIST_GROW,
        )
        .unwrap();
        (tmp, mem)
    }

    fn encoded_segment_for(value: &str) -> String {
        TachiMemory::encode_path_segment(value)
    }

    #[tokio::test]
    async fn store_recall_roundtrip_fts_with_noop_embedder() {
        let (_tmp, mem) = temp_tachi();
        mem.store(
            "lang_pref",
            "The operator prefers concise Rust answers",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let hits = mem.recall("Rust", 5, None, None, None).await.unwrap();
        assert!(
            hits.iter().any(|e| e.key == "lang_pref"),
            "expected FTS recall for keyword 'Rust', got: {hits:?}"
        );
    }

    #[tokio::test]
    async fn agent_scoping_isolates_recall_and_forget() {
        let (_tmp, mem) = temp_tachi();
        let alpha = mem.ensure_agent_uuid("alpha").await.unwrap();
        let beta = mem.ensure_agent_uuid("beta").await.unwrap();

        mem.store_with_agent(
            "note",
            "alpha-only secret token",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&alpha),
        )
        .await
        .unwrap();
        mem.store_with_agent(
            "note",
            "beta-only secret token",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some(&beta),
        )
        .await
        .unwrap();

        let alpha_hits = mem
            .recall_for_agents(&[&alpha], "alpha-only", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(alpha_hits.len(), 1);
        assert!(alpha_hits[0].content.contains("alpha-only"));

        let beta_hits = mem
            .recall_for_agents(&[&beta], "beta-only", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(beta_hits.len(), 1);
        assert!(beta_hits[0].content.contains("beta-only"));

        assert!(mem.forget_for_agent("note", &alpha).await.unwrap());
        let after = mem
            .recall_for_agents(&[&alpha], "alpha-only", 5, None, None, None)
            .await
            .unwrap();
        assert!(after.is_empty());
        let beta_still = mem
            .recall_for_agents(&[&beta], "beta-only", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(beta_still.len(), 1);
    }

    #[tokio::test]
    async fn namespace_isolation_via_recall_namespaced() {
        let (_tmp, mem) = temp_tachi();
        mem.store_with_agent(
            "hyperion_cfg",
            "hyperion namespace marker",
            MemoryCategory::Core,
            None,
            Some("hyperion"),
            None,
            None,
        )
        .await
        .unwrap();
        mem.store_with_agent(
            "default_cfg",
            "default namespace marker",
            MemoryCategory::Core,
            None,
            Some("default"),
            None,
            None,
        )
        .await
        .unwrap();

        let hyperion = mem
            .recall_namespaced("hyperion", "*", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(hyperion.len(), 1);
        assert!(hyperion[0].content.contains("hyperion namespace"));

        let default_ns = mem
            .recall_namespaced("default", "*", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(default_ns.len(), 1);
        assert!(default_ns[0].content.contains("default namespace"));
    }

    #[tokio::test]
    async fn category_mapping_roundtrip() {
        let (_tmp, mem) = temp_tachi();
        mem.store(
            "daily_note",
            "Shipped tachi backend scaffold",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();

        let entry = mem.get("daily_note").await.unwrap().expect("stored row");
        assert_eq!(entry.category, MemoryCategory::Daily);

        let listed = mem.list(Some(&MemoryCategory::Daily), None).await.unwrap();
        assert!(listed.iter().any(|e| e.key == "daily_note"));
    }

    #[tokio::test]
    async fn user_namespace_store_recall_purge_roundtrip() {
        let (_tmp, mem) = temp_tachi();
        mem.store_with_agent(
            "pref",
            "alice user-namespace fact",
            MemoryCategory::Core,
            None,
            Some("user:alice"),
            None,
            None,
        )
        .await
        .unwrap();

        let hits = mem
            .recall_namespaced("user:alice", "*", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].namespace, "user:alice");
        assert_eq!(hits[0].key, "pref");

        let purged = mem.purge_namespace("user:alice").await.unwrap();
        assert_eq!(purged, 1);
        let after = mem
            .recall_namespaced("user:alice", "*", 5, None, None, None)
            .await
            .unwrap();
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn key_collision_distinct_identities() {
        let (_tmp, mem) = temp_tachi();
        mem.store("a/b", "slash key content", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("a_b", "underscore key content", MemoryCategory::Core, None)
            .await
            .unwrap();

        let slash = mem.get("a/b").await.unwrap().expect("slash key");
        let under = mem.get("a_b").await.unwrap().expect("underscore key");
        assert_eq!(slash.content, "slash key content");
        assert_eq!(under.content, "underscore key content");
        assert_ne!(slash.id, under.id);
        assert_eq!(mem.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn export_agent_then_purge_returns_entries() {
        let (_tmp, mem) = temp_tachi();
        mem.store_with_agent(
            "k1",
            "agent alpha row one",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();
        mem.store_with_agent(
            "k2",
            "agent alpha row two",
            MemoryCategory::Daily,
            None,
            None,
            None,
            Some("alpha"),
        )
        .await
        .unwrap();

        let exported = mem.export_agent("alpha").await.unwrap();
        assert_eq!(exported.len(), 2);
        assert!(exported.iter().any(|e| e.key == "k1"));
        assert!(exported.iter().any(|e| e.key == "k2"));

        let purged = mem.purge_agent("alpha").await.unwrap();
        assert_eq!(purged, 2);
        assert!(mem.export_agent("alpha").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_session_removes_only_that_session() {
        let (_tmp, mem) = temp_tachi();
        mem.store(
            "s1",
            "session one row",
            MemoryCategory::Core,
            Some("sess-a"),
        )
        .await
        .unwrap();
        mem.store(
            "s2",
            "session two row",
            MemoryCategory::Core,
            Some("sess-b"),
        )
        .await
        .unwrap();

        let purged = mem.purge_session("sess-a").await.unwrap();
        assert_eq!(purged, 1);
        assert!(mem.get("s1").await.unwrap().is_none());
        assert!(mem.get("s2").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn rename_agent_moves_entries() {
        let (_tmp, mem) = temp_tachi();
        mem.store_with_agent(
            "note",
            "owned by old alias",
            MemoryCategory::Core,
            None,
            None,
            None,
            Some("old_alias"),
        )
        .await
        .unwrap();

        let moved = mem.rename_agent("old_alias", "new_alias").await.unwrap();
        assert_eq!(moved, 1);

        assert!(
            mem.recall_for_agents(&["old_alias"], "owned", 5, None, None, None)
                .await
                .unwrap()
                .is_empty()
        );
        let hits = mem
            .recall_for_agents(&["new_alias"], "owned", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].agent_id.as_deref(), Some("new_alias"));
        assert_eq!(hits[0].key, "note");
    }

    #[tokio::test]
    async fn get_and_forget_survive_small_list_page_cap() {
        let (_tmp, mem) = temp_tachi_small_page();
        // With page_size=2, a naive single list_by_path call would truncate.
        // Growing scan + metadata key lookup must still find every row.
        for i in 0..5 {
            mem.store(
                &format!("key_{i}"),
                &format!("content number {i} unique"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }
        assert_eq!(mem.count().await.unwrap(), 5);

        let hit = mem.get("key_4").await.unwrap().expect("key_4 visible");
        assert!(hit.content.contains("number 4"));

        assert!(mem.forget("key_0").await.unwrap());
        assert!(mem.get("key_0").await.unwrap().is_none());
        assert_eq!(mem.count().await.unwrap(), 4);
        assert!(mem.get("key_3").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn sanitize_collision_slash_vs_colon_remain_distinct() {
        let (_tmp, mem) = temp_tachi();
        // Both sanitize to `a_b` under the old non-injective scheme.
        mem.store("a/b", "slash identity", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("a:b", "colon identity", MemoryCategory::Core, None)
            .await
            .unwrap();

        let slash = mem.get("a/b").await.unwrap().expect("a/b");
        let colon = mem.get("a:b").await.unwrap().expect("a:b");
        assert_eq!(slash.content, "slash identity");
        assert_eq!(colon.content, "colon identity");
        assert_ne!(slash.id, colon.id);
        assert_ne!(
            encoded_segment_for("a/b"),
            encoded_segment_for("a:b"),
            "injective encoding must distinguish sanitize-colliding keys"
        );
        assert_eq!(mem.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn encoded_form_literal_collision_remains_distinct() {
        let (_tmp, mem) = temp_tachi();
        mem.store("a/b", "original slash key", MemoryCategory::Core, None)
            .await
            .unwrap();
        // Literal key equal to the encoded segment of `a/b` — must not collide.
        let encoded_of_slash = encoded_segment_for("a/b");
        mem.store(
            &encoded_of_slash,
            "literal encoded-looking key",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

        let original = mem.get("a/b").await.unwrap().expect("a/b");
        let literal = mem
            .get(&encoded_of_slash)
            .await
            .unwrap()
            .expect("encoded-form literal");
        assert_eq!(original.content, "original slash key");
        assert_eq!(literal.content, "literal encoded-looking key");
        assert_ne!(original.id, literal.id);
        assert_eq!(mem.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn upsert_identity_mirrors_sqlite_agent_and_key() {
        // Sqlite: ON CONFLICT(agent_id, key) — namespace/category update in place
        // (`sqlite.rs` store_row_with_metadata). Same agent+key collapses;
        // different agents stay distinct. The pre-fix fallback matched key
        // alone under the agent prefix without verifying agent metadata and
        // could grab the wrong row when paths diverged; matching (agent, key)
        // keeps identity aligned with sqlite.
        let (_tmp, mem) = temp_tachi();

        mem.store_with_agent(
            "shared",
            "ns-alpha core",
            MemoryCategory::Core,
            None,
            Some("ns-alpha"),
            None,
            Some("agent1"),
        )
        .await
        .unwrap();
        // Same agent+key, different namespace+category → one row (last wins).
        mem.store_with_agent(
            "shared",
            "ns-beta daily",
            MemoryCategory::Daily,
            None,
            Some("ns-beta"),
            None,
            Some("agent1"),
        )
        .await
        .unwrap();

        let agent1_rows = mem.export_agent("agent1").await.unwrap();
        assert_eq!(
            agent1_rows.len(),
            1,
            "sqlite identity (agent_id, key) collapses namespace/category updates"
        );
        assert_eq!(agent1_rows[0].content, "ns-beta daily");
        assert_eq!(agent1_rows[0].namespace, "ns-beta");
        assert_eq!(agent1_rows[0].category, MemoryCategory::Daily);

        // Different agent, same key → second row.
        mem.store_with_agent(
            "shared",
            "agent2 copy",
            MemoryCategory::Core,
            None,
            Some("ns-alpha"),
            None,
            Some("agent2"),
        )
        .await
        .unwrap();
        assert_eq!(mem.export_agent("agent1").await.unwrap().len(), 1);
        assert_eq!(mem.export_agent("agent2").await.unwrap().len(), 1);
        assert_eq!(mem.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn list_prefix_complete_errors_when_max_grow_full() {
        // Seed with default caps, then reopen with page == grow == 2 so three
        // rows force a full page at the cap.
        let tmp = TempDir::new().unwrap();
        {
            let mem = TachiMemory::new("tachi", tmp.path()).unwrap();
            for i in 0..3 {
                mem.store(
                    &format!("row_{i}"),
                    &format!("payload {i}"),
                    MemoryCategory::Core,
                    None,
                )
                .await
                .unwrap();
            }
        }
        let mem = TachiMemory::with_embedder_and_page_size(
            "tachi",
            tmp.path(),
            Arc::new(super::super::embeddings::NoopEmbedding),
            0.7,
            0.3,
            2,
            2,
        )
        .unwrap();

        // count() uses memcore stats (not the growing scan); list must fail loud.
        assert_eq!(mem.count().await.unwrap(), 3);
        let err = mem
            .list(None, None)
            .await
            .expect_err("full page at max_list_grow must error");
        assert!(
            err.to_string().contains("truncated"),
            "expected truncation error, got: {err}"
        );

        // Point identity no longer depends on a growing full-entry list. The
        // complete projected metadata fallback can prove a new key is absent.
        mem.store("row_new", "payload new", MemoryCategory::Core, None)
            .await
            .expect("point upsert must not depend on full-entry list capacity");
        assert_eq!(mem.count().await.unwrap(), 4);

        // Updating an EXISTING key still works at the cap: the exact-path hit
        // short-circuits before the growing scan.
        mem.store("row_0", "payload updated", MemoryCategory::Core, None)
            .await
            .expect("exact-path update must not require the fallback scan");
    }
}
