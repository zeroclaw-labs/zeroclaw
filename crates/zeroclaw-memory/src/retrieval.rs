//! Optional read-cache decorator over a single backend recall.
//!
//! [`RetrievalPipeline`] wraps a `Memory` trait object and composes as an
//! outermost decorator around any backend (or wrapper stack). Its only
//! behavioral add-on is an optional in-process hot cache over recall results.
//! When the cache is off the decorator is a transparent pass-through and
//! recall is byte-identical to the inner backend.
//!
//! `Memory` exposes a single hybrid backend-recall operation today, so the
//! decorator makes exactly one backend recall per query. It does NOT split
//! that call into distinct FTS and vector stages: `[memory] retrieval_stages`
//! only toggles the hot cache (via a `"cache"` entry), and the `"fts"` /
//! `"vector"` names plus `fts_early_return_score` are reserved for a future
//! backend contract that exposes those operations. They are inert today.
//!
//! Cache coherence: the hot cache is per-handle and in-process, and it is OFF
//! by default (the shipped `retrieval_stages` default carries no `"cache"`),
//! so activating the decorator does not change default per-agent recall. When
//! a caller opts the cache in, within-handle correctness is guaranteed by a
//! generation counter (a recall that began before a mutation cannot repopulate
//! the cache after that mutation cleared it) and by invalidating on every
//! mutation attempt, including a backend error. Cross-handle / cross-process
//! coherence over a shared store is intentionally out of scope and remains a
//! documented limitation of the opt-in cache.

use super::traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, MemoryStats, ProceduralMessage, StoreOptions,
};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A cached recall result.
struct CachedResult {
    entries: Vec<MemoryEntry>,
    created_at: Instant,
}

/// Cache identity for a recall request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RetrievalCacheKey {
    query: String,
    limit: usize,
    session_id: Option<String>,
    namespace: Option<String>,
    since: Option<String>,
    until: Option<String>,
}

impl RetrievalCacheKey {
    fn new(
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Self {
        Self {
            query: query.to_string(),
            limit,
            session_id: session_id.map(str::to_string),
            namespace: namespace.map(str::to_string),
            since: since.map(str::to_string),
            until: until.map(str::to_string),
        }
    }
}

/// Configuration for the [`RetrievalPipeline`] decorator.
///
/// The decorator's only knob is the optional hot cache. `cache_enabled`
/// mirrors whether `[memory] retrieval_stages` names `"cache"`; the reserved
/// `"fts"` / `"vector"` stage names and `fts_early_return_score` are inert and
/// do not reach this struct.
#[derive(Debug, Clone)]
pub struct RetrievalConfig {
    /// Enable the in-process hot cache over recall results. Off by default.
    pub cache_enabled: bool,
    /// Max entries in the hot cache.
    pub cache_max_entries: usize,
    /// TTL for cached results.
    pub cache_ttl: Duration,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            cache_enabled: false,
            cache_max_entries: 256,
            cache_ttl: Duration::from_secs(300),
        }
    }
}

/// Optional-cache decorator wrapping a `Memory` backend.
pub struct RetrievalPipeline {
    memory: Arc<dyn Memory>,
    config: RetrievalConfig,
    hot_cache: Mutex<HashMap<RetrievalCacheKey, CachedResult>>,
    /// Bumped on every cache invalidation. A recall captures the generation
    /// before it consults the backend and only stores its result if the
    /// generation is unchanged, so a read that began before a mutation cannot
    /// repopulate the cache after that mutation cleared it.
    generation: AtomicU64,
}

impl ::zeroclaw_api::attribution::Attributable for RetrievalPipeline {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        self.memory.role()
    }

    fn alias(&self) -> &str {
        self.memory.alias()
    }
}

impl RetrievalPipeline {
    pub fn new(memory: Arc<dyn Memory>, config: RetrievalConfig) -> Self {
        Self {
            memory,
            config,
            hot_cache: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    /// Build a cache key from query parameters.
    fn cache_key(
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> RetrievalCacheKey {
        RetrievalCacheKey::new(query, limit, session_id, namespace, since, until)
    }

    /// Check the hot cache for a live (non-expired) result.
    fn check_cache(&self, key: &RetrievalCacheKey) -> Option<Vec<MemoryEntry>> {
        let cache = self.hot_cache.lock();
        if let Some(cached) = cache.get(key)
            && cached.created_at.elapsed() < self.config.cache_ttl
        {
            return Some(cached.entries.clone());
        }
        None
    }

    /// Insert into the already-locked cache, evicting the oldest at capacity.
    fn insert_locked(
        cache: &mut HashMap<RetrievalCacheKey, CachedResult>,
        key: RetrievalCacheKey,
        entries: Vec<MemoryEntry>,
        max_entries: usize,
    ) {
        if cache.len() >= max_entries {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, v)| v.created_at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key {
                cache.remove(&k);
            }
        }
        cache.insert(
            key,
            CachedResult {
                entries,
                created_at: Instant::now(),
            },
        );
    }

    /// Store a recall result only if no invalidation has happened since
    /// `generation` was captured (before the backend recall). This closes the
    /// miss-versus-mutation race: a read that began before a mutation cannot
    /// repopulate the cache with its stale snapshot after that mutation cleared
    /// it. The generation is re-checked under the cache lock, and
    /// [`RetrievalPipeline::invalidate_cache`] advances the generation before
    /// it clears, so any insert that slipped in first is wiped by the clear.
    fn store_in_cache_if_current(
        &self,
        key: RetrievalCacheKey,
        entries: Vec<MemoryEntry>,
        generation: u64,
    ) {
        let mut cache = self.hot_cache.lock();
        if self.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        Self::insert_locked(&mut cache, key, entries, self.config.cache_max_entries);
    }

    /// Test helper: unconditionally prime the cache with a known snapshot
    /// (bypasses the generation guard).
    #[cfg(test)]
    fn store_in_cache(&self, key: RetrievalCacheKey, entries: Vec<MemoryEntry>) {
        let mut cache = self.hot_cache.lock();
        Self::insert_locked(&mut cache, key, entries, self.config.cache_max_entries);
    }

    /// The single recall path: an optional cache lookup, exactly one backend
    /// recall, then an optional cache store. `namespace` selects the backend
    /// operation. With the cache off this is a transparent pass-through.
    async fn cached_recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if !self.config.cache_enabled {
            return self
                .backend_recall(query, limit, session_id, namespace, since, until)
                .await;
        }

        let ck = Self::cache_key(query, limit, session_id, namespace, since, until);
        if let Some(cached) = self.check_cache(&ck) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"query": query})),
                "retrieval pipeline: cache hit for ''"
            );
            return Ok(cached);
        }

        // Capture the generation before consulting the backend so a concurrent
        // mutation turns the eventual store into a no-op (store_in_cache_if_current).
        let generation = self.generation.load(Ordering::SeqCst);
        let results = self
            .backend_recall(query, limit, session_id, namespace, since, until)
            .await?;
        if !results.is_empty() {
            self.store_in_cache_if_current(ck, results.clone(), generation);
        }
        Ok(results)
    }

    /// One hybrid backend recall; `namespace` picks the scoped variant.
    async fn backend_recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        match namespace {
            Some(ns) => {
                self.memory
                    .recall_namespaced(ns, query, limit, session_id, since, until)
                    .await
            }
            None => {
                self.memory
                    .recall(query, limit, session_id, since, until)
                    .await
            }
        }
    }

    /// Namespace-aware recall entry point (mirrors the `Memory` recall pair).
    pub async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        namespace: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.cached_recall(query, limit, session_id, namespace, since, until)
            .await
    }

    /// Invalidate the hot cache after a mutation and advance the generation so
    /// an in-flight pre-mutation recall cannot repopulate it.
    pub fn invalidate_cache(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.hot_cache.lock().clear();
    }

    /// Get the number of entries in the hot cache.
    pub fn cache_size(&self) -> usize {
        self.hot_cache.lock().len()
    }

    /// Test helper: the current cache generation.
    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Memory for RetrievalPipeline {
    fn name(&self) -> &str {
        self.memory.name()
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let result = self.memory.store(key, content, category, session_id).await;
        // Invalidate on every mutation attempt, including a backend error: an
        // uncertain outcome must not leave a possibly-stale cache in place.
        self.invalidate_cache();
        result
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.cached_recall(query, limit, session_id, None, since, until)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        self.memory.get(key).await
    }

    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        self.memory.get_for_agent(key, agent_id).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.memory.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let result = self.memory.forget(key).await;
        // Invalidate unless the backend confirmed a no-op; a backend error
        // still invalidates defensively.
        if !matches!(&result, Ok(false)) {
            self.invalidate_cache();
        }
        result
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        let result = self.memory.forget_for_agent(key, agent_id).await;
        if !matches!(&result, Ok(false)) {
            self.invalidate_cache();
        }
        result
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        let result = self.memory.purge_namespace(namespace).await;
        if !matches!(&result, Ok(0)) {
            self.invalidate_cache();
        }
        result
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        let result = self.memory.purge_session(session_id).await;
        if !matches!(&result, Ok(0)) {
            self.invalidate_cache();
        }
        result
    }

    async fn purge_session_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<usize> {
        let result = self
            .memory
            .purge_session_for_agent(session_id, agent_id)
            .await;
        if !matches!(&result, Ok(0)) {
            self.invalidate_cache();
        }
        result
    }

    async fn purge_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        let result = self.memory.purge_agent(agent_alias).await;
        if !matches!(&result, Ok(0)) {
            self.invalidate_cache();
        }
        result
    }

    async fn export_agent(&self, agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        self.memory.export_agent(agent_alias).await
    }

    async fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        let result = self.memory.rename_agent(from, to).await;
        if !matches!(&result, Ok(0)) {
            self.invalidate_cache();
        }
        result
    }

    async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        self.memory.count_agent(agent_alias).await
    }

    async fn count(&self) -> anyhow::Result<usize> {
        self.memory.count().await
    }

    async fn health_check(&self) -> bool {
        self.memory.health_check().await
    }

    async fn supersede(&self, superseded_ids: &[String], new_id: &str) -> anyhow::Result<()> {
        let result = self.memory.supersede(superseded_ids, new_id).await;
        // A real supersede (non-empty ids) or an uncertain outcome (error)
        // invalidates; an empty-id no-op that succeeded keeps the cache.
        if !superseded_ids.is_empty() || result.is_err() {
            self.invalidate_cache();
        }
        result
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let result = self.memory.store_procedural(messages, session_id).await;
        self.invalidate_cache();
        result
    }

    async fn count_in_scope(
        &self,
        namespace: Option<&str>,
        category: Option<&MemoryCategory>,
    ) -> anyhow::Result<u64> {
        self.memory.count_in_scope(namespace, category).await
    }

    async fn stats(&self) -> anyhow::Result<MemoryStats> {
        self.memory.stats().await
    }

    async fn reindex(&self) -> anyhow::Result<usize> {
        let result = self.memory.reindex().await;
        self.invalidate_cache();
        result
    }

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        self.memory
            .refresh_embedder(model_provider, api_key, model, dimensions);
        // A swapped embedder changes what vector recall would return; a
        // cached result from the old embedder must not outlive the swap.
        self.invalidate_cache();
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
        self.cached_recall(query, limit, session_id, Some(namespace), since, until)
            .await
    }

    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        self.memory.export(filter).await
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
        let result = self
            .memory
            .store_with_metadata(key, content, category, session_id, namespace, importance)
            .await;
        self.invalidate_cache();
        result
    }

    async fn store_with_options(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
    ) -> anyhow::Result<()> {
        let result = self
            .memory
            .store_with_options(key, content, category, session_id, options)
            .await;
        self.invalidate_cache();
        result
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
        let result = self
            .memory
            .store_with_agent(
                key, content, category, session_id, namespace, importance, agent_id,
            )
            .await;
        self.invalidate_cache();
        result
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
        // Cross-agent reads are not cached: a correct cache key would have to
        // canonicalize the allowlist so one agent's rows never serve another.
        // That is out of scope here, so this always hits the backend directly.
        self.memory
            .recall_for_agents(allowed_agent_ids, query, limit, session_id, since, until)
            .await
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        self.memory.ensure_agent_uuid(alias).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::none::NoneMemory;
    use crate::sqlite::SqliteMemory;
    use crate::traits::MemoryCategory;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_config::schema::SearchMode;

    /// Cache config with the opt-in hot cache enabled (the production default
    /// keeps it off).
    fn cached_config() -> RetrievalConfig {
        RetrievalConfig {
            cache_enabled: true,
            ..RetrievalConfig::default()
        }
    }

    struct StatefulMemory {
        entries: Mutex<Vec<MemoryEntry>>,
        recalls: AtomicUsize,
        /// When set, mutations return a backend error (reads still succeed).
        fail_mutations: bool,
    }

    impl StatefulMemory {
        fn new(entries: Vec<MemoryEntry>) -> Self {
            Self {
                entries: Mutex::new(entries),
                recalls: AtomicUsize::new(0),
                fail_mutations: false,
            }
        }

        /// A backend whose mutations fail; used to prove invalidate-on-error.
        fn failing(entries: Vec<MemoryEntry>) -> Self {
            Self {
                entries: Mutex::new(entries),
                recalls: AtomicUsize::new(0),
                fail_mutations: true,
            }
        }

        fn recalls(&self) -> usize {
            self.recalls.load(Ordering::SeqCst)
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for StatefulMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }

        fn alias(&self) -> &str {
            "stateful-memory"
        }
    }

    #[async_trait]
    impl Memory for StatefulMemory {
        fn name(&self) -> &str {
            "stateful"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            if self.fail_mutations {
                anyhow::bail!("backend down");
            }
            *self.entries.lock() = vec![entry(key, content, 1.0, category, session_id)];
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.recalls.fetch_add(1, Ordering::SeqCst);
            Ok(self.entries.lock().iter().take(limit).cloned().collect())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(self.entries.lock().clone())
        }

        async fn forget(&self, key: &str) -> anyhow::Result<bool> {
            let mut entries = self.entries.lock();
            let before = entries.len();
            entries.retain(|entry| entry.key != key);
            Ok(entries.len() != before)
        }

        async fn forget_for_agent(&self, key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            self.forget(key).await
        }

        async fn purge_namespace(&self, _namespace: &str) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn purge_session(&self, _session_id: &str) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn purge_session_for_agent(
            &self,
            _session_id: &str,
            _agent_id: &str,
        ) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn purge_agent(&self, _agent_alias: &str) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn rename_agent(&self, _from: &str, _to: &str) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn reindex(&self) -> anyhow::Result<usize> {
            Ok(1)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.lock().len())
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            self.store(key, content, category, session_id).await
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.recall(query, limit, session_id, since, until).await
        }
    }

    fn entry(
        key: &str,
        content: &str,
        score: f64,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> MemoryEntry {
        MemoryEntry {
            id: key.into(),
            key: key.into(),
            content: content.into(),
            category,
            timestamp: "2026-06-30T00:00:00Z".into(),
            session_id: session_id.map(str::to_string),
            score: Some(score),
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
            kind: None,
            pinned: false,
            tenant_id: None,
            agent_alias: None,
            agent_id: None,
        }
    }

    /// Serialize entries so comparisons cover every field, byte for byte.
    fn as_bytes(entries: &[MemoryEntry]) -> String {
        serde_json::to_string(entries).expect("MemoryEntry serializes")
    }

    /// Deterministic embedder so the sqlite hybrid (FTS + vector) path is
    /// reproducible without a network.
    struct DetEmbedding;

    #[async_trait]
    impl crate::embeddings::EmbeddingProvider for DetEmbedding {
        fn name(&self) -> &str {
            "det"
        }

        fn dimensions(&self) -> usize {
            8
        }

        async fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|text| {
                    let seed = text.bytes().fold(0u32, |acc, b| {
                        acc.wrapping_mul(31).wrapping_add(u32::from(b))
                    });
                    (0..8u32)
                        .map(|i| ((seed.wrapping_add(i.wrapping_mul(7919)) % 1000) as f32) / 1000.0)
                        .collect()
                })
                .collect())
        }
    }

    /// A sqlite backend with several entries across sessions and namespaces,
    /// exercising the real FTS + vector hybrid merge.
    async fn seeded_sqlite(dir: &std::path::Path) -> Arc<SqliteMemory> {
        let mem = SqliteMemory::with_embedder(
            "sqlite",
            dir,
            Arc::new(DetEmbedding),
            0.7,
            0.3,
            1000,
            None,
            SearchMode::default(),
        )
        .unwrap();
        mem.store(
            "deploy-target",
            "alpha cluster serves production deploys",
            MemoryCategory::Core,
            Some("sess-a"),
        )
        .await
        .unwrap();
        mem.store(
            "deploy-runbook",
            "the deploy runbook lives in the ops repo",
            MemoryCategory::Core,
            Some("sess-b"),
        )
        .await
        .unwrap();
        mem.store(
            "db-note",
            "postgres database has two read replicas",
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "deploy-freeze",
            "deploy freeze every friday afternoon",
            MemoryCategory::Core,
            Some("sess-a"),
        )
        .await
        .unwrap();
        mem.store_with_metadata(
            "ns-fact",
            "namespaced deploy fact for isolation",
            MemoryCategory::Core,
            None,
            Some("ns1"),
            Some(0.8),
        )
        .await
        .unwrap();
        Arc::new(mem)
    }

    #[tokio::test]
    async fn pipeline_returns_empty_from_none_backend() {
        let memory = Arc::new(NoneMemory::new("none"));
        let pipeline = RetrievalPipeline::new(memory, RetrievalConfig::default());

        let results = pipeline
            .recall("test", 10, None, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn pipeline_cache_invalidation() {
        let memory = Arc::new(NoneMemory::new("none"));
        let pipeline = RetrievalPipeline::new(memory, RetrievalConfig::default());

        // Force a cache entry
        let ck = RetrievalPipeline::cache_key("test", 10, None, None, None, None);
        pipeline.store_in_cache(ck, vec![]);

        assert_eq!(pipeline.cache_size(), 1);
        pipeline.invalidate_cache();
        assert_eq!(pipeline.cache_size(), 0);
    }

    #[test]
    fn cache_key_includes_all_params() {
        let base = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-a"),
            Some("ns1"),
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        let different_query = RetrievalPipeline::cache_key(
            "goodbye",
            10,
            Some("sess-a"),
            Some("ns1"),
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        let different_limit = RetrievalPipeline::cache_key(
            "hello",
            11,
            Some("sess-a"),
            Some("ns1"),
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        let different_session = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-b"),
            Some("ns1"),
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        let different_namespace = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-a"),
            Some("ns2"),
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        let different_since = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-a"),
            Some("ns1"),
            Some("2026-06-03T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        let different_until = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-a"),
            Some("ns1"),
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-04T00:00:00Z"),
        );
        let absent_since = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-a"),
            Some("ns1"),
            None,
            Some("2026-06-02T00:00:00Z"),
        );
        let empty_since = RetrievalPipeline::cache_key(
            "hello",
            10,
            Some("sess-a"),
            Some("ns1"),
            Some(""),
            Some("2026-06-02T00:00:00Z"),
        );
        let delimiter_in_query =
            RetrievalPipeline::cache_key("hello:10", 20, None, None, None, None);
        let delimiter_in_limit_shape =
            RetrievalPipeline::cache_key("hello", 10, Some("20"), None, None, None);

        assert_ne!(base, different_query);
        assert_ne!(base, different_limit);
        assert_ne!(base, different_session);
        assert_ne!(base, different_namespace);
        assert_ne!(base, different_since);
        assert_ne!(base, different_until);
        assert_ne!(absent_since, empty_since);
        assert_ne!(delimiter_in_query, delimiter_in_limit_shape);
    }

    #[tokio::test]
    async fn retrieval_cache_distinguishes_time_windows() {
        let memory = Arc::new(NoneMemory::new("none"));
        let pipeline = RetrievalPipeline::new(memory, cached_config());
        let cached_entry = entry(
            "project",
            "cached content",
            0.9,
            MemoryCategory::Core,
            Some("session-a"),
        );
        let first_window_key = RetrievalPipeline::cache_key(
            "project",
            10,
            Some("session-a"),
            None,
            Some("2026-06-01T00:00:00Z"),
            Some("2026-06-02T00:00:00Z"),
        );
        pipeline.store_in_cache(first_window_key, vec![cached_entry]);

        let first = pipeline
            .recall(
                "project",
                10,
                Some("session-a"),
                None,
                Some("2026-06-01T00:00:00Z"),
                Some("2026-06-02T00:00:00Z"),
            )
            .await
            .unwrap();
        let second = pipeline
            .recall(
                "project",
                10,
                Some("session-a"),
                None,
                Some("2026-06-03T00:00:00Z"),
                Some("2026-06-04T00:00:00Z"),
            )
            .await
            .unwrap();

        assert_eq!(first[0].content, "cached content");
        assert!(
            second.is_empty(),
            "a different time window must not reuse a cached result"
        );
    }

    #[tokio::test]
    async fn pipeline_caches_results() {
        let memory = Arc::new(NoneMemory::new("none"));
        let pipeline = RetrievalPipeline::new(memory, cached_config());

        // First call: cache miss, no results
        let results = pipeline
            .recall("test", 10, None, None, None, None)
            .await
            .unwrap();
        assert!(results.is_empty());

        // Manually insert a cache entry
        let ck = RetrievalPipeline::cache_key("cached_query", 5, None, None, None, None);
        let fake_entry = entry("k", "cached content", 0.9, MemoryCategory::Core, None);
        pipeline.store_in_cache(ck, vec![fake_entry]);

        // Cache hit
        let results = pipeline
            .recall("cached_query", 5, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "cached content");
    }

    // -- Neutrality: pipeline-at-defaults == direct backend recall --------

    #[tokio::test]
    async fn defaults_identity_on_sqlite_hybrid_recall() {
        let tmp = tempfile::TempDir::new().unwrap();
        let backend = seeded_sqlite(tmp.path()).await;

        // (query, session, since, until) combinations covering keyword,
        // session-scoped, recent-wildcard, and time-window recalls.
        type RecallCase<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);
        let cases: &[RecallCase<'_>] = &[
            ("deploy", None, None, None),
            ("deploy", Some("sess-a"), None, None),
            ("postgres replicas", None, None, None),
            ("*", None, None, None),
            (
                "deploy",
                None,
                Some("2020-01-01T00:00:00Z"),
                Some("2100-01-01T00:00:00Z"),
            ),
        ];

        for (query, session, since, until) in cases {
            let direct = backend
                .recall(query, 10, *session, *since, *until)
                .await
                .unwrap();
            // At the shipped defaults the decorator does not cache, so recall
            // is a pass-through and must be byte-identical to direct recall.
            let pipeline = RetrievalPipeline::new(
                backend.clone() as Arc<dyn Memory>,
                RetrievalConfig::default(),
            );
            let staged = Memory::recall(&pipeline, query, 10, *session, *since, *until)
                .await
                .unwrap();
            assert_eq!(
                as_bytes(&staged),
                as_bytes(&direct),
                "pipeline-at-defaults must be byte-identical to direct recall \
                 (same rows, order, scores) for query {query:?} session {session:?}"
            );

            // With the opt-in cache on, the first recall populates it and the
            // second is a cache hit; both must be byte-identical to direct.
            let cached_pipeline =
                RetrievalPipeline::new(backend.clone() as Arc<dyn Memory>, cached_config());
            let miss = Memory::recall(&cached_pipeline, query, 10, *session, *since, *until)
                .await
                .unwrap();
            let hit = Memory::recall(&cached_pipeline, query, 10, *session, *since, *until)
                .await
                .unwrap();
            assert_eq!(as_bytes(&miss), as_bytes(&direct));
            assert_eq!(
                as_bytes(&hit),
                as_bytes(&direct),
                "cache-hit recall must be byte-identical to direct recall for query {query:?}"
            );
        }

        // Unscoped keyword recall must actually match rows, otherwise the
        // identity assertions above prove nothing.
        let direct = backend
            .recall("deploy", 10, None, None, None)
            .await
            .unwrap();
        assert!(
            direct.len() >= 2,
            "fixture must produce multi-entry recalls"
        );
    }

    #[tokio::test]
    async fn defaults_identity_on_namespaced_recall() {
        let tmp = tempfile::TempDir::new().unwrap();
        let backend = seeded_sqlite(tmp.path()).await;
        let pipeline = RetrievalPipeline::new(
            backend.clone() as Arc<dyn Memory>,
            RetrievalConfig::default(),
        );

        let direct = backend
            .recall_namespaced("ns1", "deploy fact", 10, None, None, None)
            .await
            .unwrap();
        let staged =
            Memory::recall_namespaced(&pipeline, "ns1", "deploy fact", 10, None, None, None)
                .await
                .unwrap();
        assert!(!direct.is_empty(), "namespaced fixture must match");
        assert_eq!(as_bytes(&staged), as_bytes(&direct));

        // A repeat namespaced recall is uncached at defaults, so it is another
        // direct backend call and must stay byte-identical.
        let repeat =
            Memory::recall_namespaced(&pipeline, "ns1", "deploy fact", 10, None, None, None)
                .await
                .unwrap();
        assert_eq!(as_bytes(&repeat), as_bytes(&direct));
    }

    #[tokio::test]
    async fn memory_decorator_preserves_backend_results_without_activating_stages() {
        let above = Arc::new(StatefulMemory::new(vec![
            entry("hot", "high scoring row", 0.95, MemoryCategory::Core, None),
            entry("warm", "second row", 0.60, MemoryCategory::Core, None),
        ]));
        let direct = above.recall("query", 5, None, None, None).await.unwrap();
        let pipeline =
            RetrievalPipeline::new(above.clone() as Arc<dyn Memory>, RetrievalConfig::default());
        let decorated = Memory::recall(&pipeline, "query", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(as_bytes(&decorated), as_bytes(&direct));
        assert_eq!(above.recalls(), 2);
        assert_eq!(
            pipeline.cache_size(),
            0,
            "the decorator must not enable cache"
        );

        let below = Arc::new(StatefulMemory::new(vec![
            entry(
                "mid",
                "medium scoring row",
                0.50,
                MemoryCategory::Core,
                None,
            ),
            entry("low", "low scoring row", 0.20, MemoryCategory::Core, None),
        ]));
        let direct = below.recall("query", 5, None, None, None).await.unwrap();
        let pipeline =
            RetrievalPipeline::new(below.clone() as Arc<dyn Memory>, RetrievalConfig::default());
        let decorated = Memory::recall(&pipeline, "query", 5, None, None, None)
            .await
            .unwrap();
        assert_eq!(as_bytes(&decorated), as_bytes(&direct));
        assert_eq!(below.recalls(), 2);
        assert_eq!(
            pipeline.cache_size(),
            0,
            "the decorator must not enable cache"
        );
    }

    #[tokio::test]
    async fn empty_recall_is_identical_and_uncached() {
        let memory = Arc::new(StatefulMemory::new(vec![]));
        let pipeline = RetrievalPipeline::new(
            memory.clone() as Arc<dyn Memory>,
            RetrievalConfig::default(),
        );

        let staged = Memory::recall(&pipeline, "query", 5, None, None, None)
            .await
            .unwrap();
        let direct = memory.recall("query", 5, None, None, None).await.unwrap();
        assert_eq!(as_bytes(&staged), as_bytes(&direct));
        assert!(staged.is_empty());
        // Empty results are never cached, so a later write is always visible.
        assert_eq!(pipeline.cache_size(), 0);
    }

    #[tokio::test]
    async fn pipeline_checks_cache_before_backend_stage() {
        let memory = Arc::new(StatefulMemory::new(vec![entry(
            "k1",
            "first backend result",
            0.9,
            MemoryCategory::Core,
            None,
        )]));
        let pipeline = RetrievalPipeline::new(memory.clone(), cached_config());

        let first = pipeline
            .recall("query", 5, None, None, None, None)
            .await
            .unwrap();
        let second = pipeline
            .recall("query", 5, None, None, None, None)
            .await
            .unwrap();

        assert_eq!(first[0].content, "first backend result");
        assert_eq!(as_bytes(&second), as_bytes(&first));
        assert_eq!(memory.recalls(), 1, "second recall should hit hot cache");
    }

    #[tokio::test]
    async fn pipeline_store_invalidates_cached_recall() {
        let memory = Arc::new(StatefulMemory::new(vec![entry(
            "old",
            "stale cached result",
            0.9,
            MemoryCategory::Core,
            None,
        )]));
        let pipeline = RetrievalPipeline::new(memory.clone(), cached_config());

        let first = pipeline
            .recall("query", 5, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(first[0].content, "stale cached result");
        assert_eq!(pipeline.cache_size(), 1);

        pipeline
            .store("new", "fresh written result", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(pipeline.cache_size(), 0);

        let second = pipeline
            .recall("query", 5, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(second[0].content, "fresh written result");
        assert_eq!(memory.recalls(), 2);
    }

    /// Prime the pipeline's hot cache with one non-empty recall.
    async fn primed_pipeline() -> RetrievalPipeline {
        let memory = Arc::new(StatefulMemory::new(vec![entry(
            "seed",
            "seed content",
            0.9,
            MemoryCategory::Core,
            Some("sess"),
        )]));
        let pipeline = RetrievalPipeline::new(memory as Arc<dyn Memory>, cached_config());
        let got = pipeline
            .recall("seed", 5, None, None, None, None)
            .await
            .unwrap();
        assert!(!got.is_empty());
        assert_eq!(pipeline.cache_size(), 1);
        pipeline
    }

    #[tokio::test]
    async fn every_mutator_invalidates_cached_recall() {
        let p = primed_pipeline().await;
        p.store("k", "c", MemoryCategory::Core, None).await.unwrap();
        assert_eq!(p.cache_size(), 0, "store must invalidate");

        let p = primed_pipeline().await;
        p.store_with_metadata("k", "c", MemoryCategory::Core, None, Some("ns"), Some(0.5))
            .await
            .unwrap();
        assert_eq!(p.cache_size(), 0, "store_with_metadata must invalidate");

        let p = primed_pipeline().await;
        p.store_with_options(
            "k",
            "c",
            MemoryCategory::Core,
            None,
            StoreOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(p.cache_size(), 0, "store_with_options must invalidate");

        let p = primed_pipeline().await;
        p.store_with_agent("k", "c", MemoryCategory::Core, None, None, None, Some("a1"))
            .await
            .unwrap();
        assert_eq!(p.cache_size(), 0, "store_with_agent must invalidate");

        let p = primed_pipeline().await;
        assert!(p.forget("seed").await.unwrap());
        assert_eq!(p.cache_size(), 0, "forget (hit) must invalidate");

        let p = primed_pipeline().await;
        assert!(p.forget_for_agent("seed", "a1").await.unwrap());
        assert_eq!(p.cache_size(), 0, "forget_for_agent (hit) must invalidate");

        let p = primed_pipeline().await;
        assert_eq!(p.purge_namespace("ns").await.unwrap(), 1);
        assert_eq!(p.cache_size(), 0, "purge_namespace must invalidate");

        let p = primed_pipeline().await;
        assert_eq!(p.purge_session("sess").await.unwrap(), 1);
        assert_eq!(p.cache_size(), 0, "purge_session must invalidate");

        let p = primed_pipeline().await;
        assert_eq!(p.purge_session_for_agent("sess", "a1").await.unwrap(), 1);
        assert_eq!(p.cache_size(), 0, "purge_session_for_agent must invalidate");

        let p = primed_pipeline().await;
        assert_eq!(p.purge_agent("alias").await.unwrap(), 1);
        assert_eq!(p.cache_size(), 0, "purge_agent must invalidate");

        let p = primed_pipeline().await;
        assert_eq!(p.rename_agent("old", "new").await.unwrap(), 1);
        assert_eq!(p.cache_size(), 0, "rename_agent must invalidate");

        let p = primed_pipeline().await;
        p.supersede(&["seed".to_string()], "new-id").await.unwrap();
        assert_eq!(p.cache_size(), 0, "supersede must invalidate");

        let p = primed_pipeline().await;
        p.reindex().await.unwrap();
        assert_eq!(p.cache_size(), 0, "reindex must invalidate");

        let p = primed_pipeline().await;
        p.store_procedural(&[], None).await.unwrap();
        assert_eq!(p.cache_size(), 0, "store_procedural must invalidate");

        let p = primed_pipeline().await;
        Memory::refresh_embedder(&p, "openai", None, "model", 8);
        assert_eq!(p.cache_size(), 0, "refresh_embedder must invalidate");
    }

    #[tokio::test]
    async fn non_mutating_misses_keep_the_cache() {
        // A forget that removed nothing must not blow the cache.
        let p = primed_pipeline().await;
        assert!(!p.forget("absent").await.unwrap());
        assert_eq!(p.cache_size(), 1, "forget miss must not invalidate");

        // A supersede with no superseded ids is a no-op.
        let p = primed_pipeline().await;
        p.supersede(&[], "new-id").await.unwrap();
        assert_eq!(p.cache_size(), 1, "empty supersede must not invalidate");
    }

    #[tokio::test]
    async fn pipeline_preserves_backend_sorted_scores() {
        let memory = Arc::new(StatefulMemory::new(vec![
            entry(
                "high",
                "normalized fused high score",
                1.0,
                MemoryCategory::Core,
                None,
            ),
            entry(
                "mid",
                "normalized fused medium score",
                0.6,
                MemoryCategory::Core,
                None,
            ),
            entry(
                "low",
                "normalized fused low score",
                0.2,
                MemoryCategory::Core,
                None,
            ),
        ]));
        let pipeline = RetrievalPipeline::new(memory, RetrievalConfig::default());

        let results = pipeline
            .recall("query", 3, None, None, None, None)
            .await
            .unwrap();

        assert_eq!(
            results
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "mid", "low"]
        );
        assert!(results.iter().all(|entry| {
            entry
                .score
                .is_some_and(|score| (0.0..=1.0).contains(&score))
        }));
    }

    /// Concurrent miss-versus-write: a recall that captured its generation
    /// before a mutation cannot repopulate the cache after that mutation
    /// invalidated it. Exercises the generation guard deterministically.
    #[tokio::test]
    async fn stale_read_cannot_repopulate_after_invalidation() {
        let memory = Arc::new(StatefulMemory::new(vec![entry(
            "seed",
            "value",
            0.9,
            MemoryCategory::Core,
            None,
        )]));
        let pipeline = RetrievalPipeline::new(memory, cached_config());
        let key = RetrievalPipeline::cache_key("q", 5, None, None, None, None);

        // A recall captures the generation before consulting the backend.
        let stale_gen = pipeline.generation();
        // A concurrent mutation invalidates and advances the generation.
        pipeline.invalidate_cache();
        // The pre-mutation read now tries to store its snapshot: dropped.
        pipeline.store_in_cache_if_current(
            key.clone(),
            vec![entry("seed", "value", 0.9, MemoryCategory::Core, None)],
            stale_gen,
        );
        assert_eq!(
            pipeline.cache_size(),
            0,
            "a pre-mutation read must not repopulate the cache"
        );

        // A read that starts after the invalidation stores normally.
        let fresh_gen = pipeline.generation();
        pipeline.store_in_cache_if_current(
            key,
            vec![entry("seed", "value", 0.9, MemoryCategory::Core, None)],
            fresh_gen,
        );
        assert_eq!(
            pipeline.cache_size(),
            1,
            "a current read populates the cache"
        );
    }

    /// Partial failure: a mutation whose backend errors must still invalidate
    /// the cache rather than leave a possibly-stale snapshot behind.
    #[tokio::test]
    async fn failed_mutation_still_invalidates() {
        let memory = Arc::new(StatefulMemory::failing(vec![entry(
            "seed",
            "value",
            0.9,
            MemoryCategory::Core,
            None,
        )]));
        let pipeline = RetrievalPipeline::new(memory, cached_config());

        // Prime the cache with a successful recall (reads do not fail).
        let primed = pipeline
            .recall("q", 5, None, None, None, None)
            .await
            .unwrap();
        assert_eq!(primed.len(), 1);
        assert_eq!(pipeline.cache_size(), 1);

        // The mutation fails at the backend; the cache must still be dropped.
        let result = pipeline.store("k", "c", MemoryCategory::Core, None).await;
        assert!(result.is_err(), "the backend error must propagate");
        assert_eq!(
            pipeline.cache_size(),
            0,
            "a failed mutation must still invalidate the cache"
        );
    }
}
