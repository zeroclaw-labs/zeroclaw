//! Three-tier memory system: STM (short-term), MTM (medium-term), LTM (long-term).
//!
//! [`TieredMemory`] wraps three [`Memory`] backends and implements the
//! [`Memory`] trait itself, acting as a drop-in replacement for any single
//! backend while adding tiered recall, merge-and-rank, and lifecycle commands.

pub mod budget;
pub mod extractor;
pub mod extraction_worker;
pub mod facts;
pub mod prompts;
pub mod loader;
pub mod merge;
pub mod summarization;
pub mod tagging;
pub mod timezone;
pub mod manager;
pub mod types;

#[allow(unused_imports)]
pub use types::{
    CompressionJob, CompressionJobKind, CompressionJobStatus, IndexEntry, MemoryTier, TierCommand,
    TierConfig,
};

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, Mutex};

use crate::memory::tiered::merge::{merge_and_rank, FactConfidenceLevel, TierWeights, TieredRecallItem};

use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};

/// A request to extract facts from a conversation turn.
#[derive(Debug, Clone)]
pub struct ExtractionRequest {
    pub content: String,
    pub role: String,       // "user" or "agent"
    pub key: String,
    pub session_id: Option<String>,
    pub timestamp_unix_ms: i64,
}

/// Shared handle to a [`Memory`] backend, safe for concurrent access.
pub type SharedMemory = Arc<Mutex<Box<dyn Memory + Send>>>;

/// Central three-tier memory struct.
///
/// Wraps short-term (STM), medium-term (MTM), and long-term (LTM) backends
/// and exposes them through the unified [`Memory`] trait.
pub struct TieredMemory {
    pub stm: SharedMemory,
    pub mtm: SharedMemory,
    pub ltm: SharedMemory,
    pub cfg: Arc<TierConfig>,
    pub cmd_tx: mpsc::Sender<TierCommand>,
    pub extraction_tx: Option<mpsc::Sender<ExtractionRequest>>,
}

/// Compute a recency score in [0.0, 1.0] from an ISO-8601 timestamp string.
///
/// Returns 1.0 for entries created right now, decaying linearly to 0.0 at 24 h.
/// Unparseable timestamps default to 0.0.
fn recency_score_from_timestamp(timestamp: &str) -> f32 {
    let parsed = timestamp
        .parse::<DateTime<Utc>>()
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|ndt| ndt.and_utc())
        });

    match parsed {
        Some(dt) => {
            let age_secs = Utc::now()
                .signed_duration_since(dt)
                .num_seconds()
                .max(0) as f32;
            (1.0 - age_secs / 86400.0).max(0.0)
        }
        None => 0.0,
    }
}

#[async_trait]
impl Memory for TieredMemory {
    fn name(&self) -> &str {
        "tiered"
    }

    /// Store writes to STM only; then enqueues a non-blocking extraction
    /// request when an extraction channel is configured.
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.stm
            .lock()
            .await
            .store(key, content, category, session_id)
            .await?;

        // Non-blocking extraction enqueue (best-effort).
        if let Some(ref tx) = self.extraction_tx {
            let role = if key.starts_with("msg:agent:") { "agent" } else { "user" };
            let req = ExtractionRequest {
                content: content.to_string(),
                role: role.to_string(),
                key: key.to_string(),
                session_id: session_id.map(|s| s.to_string()),
                timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
            };
            let _ = tx.try_send(req); // Non-blocking, drop if queue full
        }

        Ok(())
    }

    /// Recall: fetch from all 3 tiers concurrently, merge and rank.
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let k = self.cfg.recall_top_k_per_tier;

        // 1. Fetch concurrently from all three tiers.
        let (stm_res, mtm_res, ltm_res) = tokio::join!(
            async { self.stm.lock().await.recall(query, k, session_id).await },
            async { self.mtm.lock().await.recall(query, k, session_id).await },
            async { self.ltm.lock().await.recall(query, k, session_id).await },
        );

        // Collect results, ignoring tier errors (treat as empty).
        let stm_entries = stm_res.unwrap_or_default();
        let mtm_entries = mtm_res.unwrap_or_default();
        let ltm_entries = ltm_res.unwrap_or_default();

        // 2. Build a lookup map: entry_id -> MemoryEntry.
        let mut entry_map: HashMap<String, MemoryEntry> = HashMap::new();
        let mut items: Vec<TieredRecallItem> = Vec::new();

        let tiers = [
            (MemoryTier::Stm, &stm_entries),
            (MemoryTier::Mtm, &mtm_entries),
            (MemoryTier::Ltm, &ltm_entries),
        ];

        for (tier, entries) in &tiers {
            for entry in *entries {
                let base_score = entry.score.unwrap_or(0.5) as f32;
                let recency = recency_score_from_timestamp(&entry.timestamp);

                let is_fact = entry.key.starts_with("fact:");
                let fact_confidence = if is_fact {
                    serde_json::from_str::<crate::memory::tiered::facts::FactEntry>(&entry.content)
                        .ok()
                        .map(|f| match f.confidence {
                            crate::memory::tiered::facts::FactConfidence::High => FactConfidenceLevel::High,
                            crate::memory::tiered::facts::FactConfidence::Medium => FactConfidenceLevel::Medium,
                            crate::memory::tiered::facts::FactConfidence::Low => FactConfidenceLevel::Low,
                        })
                } else {
                    None
                };

                items.push(TieredRecallItem {
                    entry_id: entry.id.clone(),
                    origin_id: entry.id.clone(),
                    tier: *tier,
                    base_score,
                    recency_score: recency,
                    has_cross_tier_link: false,
                    final_score: 0.0,
                    is_fact,
                    fact_confidence,
                });

                entry_map.insert(entry.id.clone(), entry.clone());
            }
        }

        // 3. Merge and rank.
        let weights = TierWeights {
            stm: self.cfg.weight_stm,
            mtm: self.cfg.weight_mtm,
            ltm: self.cfg.weight_ltm,
        };
        let final_k = self.cfg.recall_final_top_k.min(limit);
        let ranked = merge_and_rank(items, &weights, self.cfg.min_relevance_threshold, final_k);

        // 4. Map ranked items back to MemoryEntry values.
        let results: Vec<MemoryEntry> = ranked
            .into_iter()
            .filter_map(|item| {
                entry_map.remove(&item.entry_id).map(|mut e| {
                    e.score = Some(item.final_score as f64);
                    e
                })
            })
            .collect();

        Ok(results)
    }

    /// Get: check STM, then MTM, then LTM (first match wins).
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        if let Some(entry) = self.stm.lock().await.get(key).await? {
            return Ok(Some(entry));
        }
        if let Some(entry) = self.mtm.lock().await.get(key).await? {
            return Ok(Some(entry));
        }
        self.ltm.lock().await.get(key).await
    }

    /// List: returns STM entries only (always injected into prompts).
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.stm.lock().await.list(category, session_id).await
    }

    /// Forget: removes from all 3 tiers (ignore individual errors).
    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let stm = self.stm.lock().await.forget(key).await.unwrap_or(false);
        let mtm = self.mtm.lock().await.forget(key).await.unwrap_or(false);
        let ltm = self.ltm.lock().await.forget(key).await.unwrap_or(false);
        Ok(stm || mtm || ltm)
    }

    /// Count: sum of counts from all 3 tiers.
    async fn count(&self) -> anyhow::Result<usize> {
        let stm = self.stm.lock().await.count().await?;
        let mtm = self.mtm.lock().await.count().await?;
        let ltm = self.ltm.lock().await.count().await?;
        Ok(stm + mtm + ltm)
    }

    /// Health check: all 3 tiers must pass.
    async fn health_check(&self) -> bool {
        let stm_ok = self.stm.lock().await.health_check().await;
        let mtm_ok = self.mtm.lock().await.health_check().await;
        let ltm_ok = self.ltm.lock().await.health_check().await;
        stm_ok && mtm_ok && ltm_ok
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    // ── InMemoryBackend ───────────────────────────────────────────────────

    /// Simple HashMap-backed Memory implementation for testing.
    /// Uses interior mutability (std::sync::Mutex) because the Memory trait
    /// requires `&self` for store/forget.
    struct InMemoryBackend {
        entries: std::sync::Mutex<HashMap<String, MemoryEntry>>,
    }

    impl InMemoryBackend {
        fn new() -> Self {
            Self {
                entries: std::sync::Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for InMemoryBackend {
        fn name(&self) -> &str {
            "in-memory-test"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            let entry = MemoryEntry {
                id: key.to_string(),
                key: key.to_string(),
                content: content.to_string(),
                category,
                timestamp: Utc::now().to_rfc3339(),
                session_id: session_id.map(String::from),
                score: None,
            };
            self.entries.lock().unwrap().insert(key.to_string(), entry);
            Ok(())
        }

        async fn recall(
            &self,
            query: &str,
            limit: usize,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let guard = self.entries.lock().unwrap();
            let results: Vec<MemoryEntry> = guard
                .values()
                .filter(|e| e.content.contains(query) || e.key.contains(query))
                .take(limit)
                .cloned()
                .collect();
            Ok(results)
        }

        async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(self.entries.lock().unwrap().get(key).cloned())
        }

        async fn list(
            &self,
            category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            let guard = self.entries.lock().unwrap();
            let results: Vec<MemoryEntry> = guard
                .values()
                .filter(|e| category.map_or(true, |c| &e.category == c))
                .cloned()
                .collect();
            Ok(results)
        }

        async fn forget(&self, key: &str) -> anyhow::Result<bool> {
            Ok(self.entries.lock().unwrap().remove(key).is_some())
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.lock().unwrap().len())
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_shared(backend: InMemoryBackend) -> SharedMemory {
        Arc::new(Mutex::new(Box::new(backend)))
    }

    async fn make_test_tiered() -> TieredMemory {
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        TieredMemory {
            stm: make_shared(InMemoryBackend::new()),
            mtm: make_shared(InMemoryBackend::new()),
            ltm: make_shared(InMemoryBackend::new()),
            cfg: Arc::new(TierConfig::default()),
            cmd_tx,
            extraction_tx: None,
        }
    }

    /// Helper to create a TieredMemory with a custom min_relevance_threshold.
    async fn make_test_tiered_with_threshold(threshold: f32) -> TieredMemory {
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let mut cfg = TierConfig::default();
        cfg.min_relevance_threshold = threshold;
        TieredMemory {
            stm: make_shared(InMemoryBackend::new()),
            mtm: make_shared(InMemoryBackend::new()),
            ltm: make_shared(InMemoryBackend::new()),
            cfg: Arc::new(cfg),
            cmd_tx,
            extraction_tx: None,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn store_writes_to_stm_only() {
        let tiered = make_test_tiered().await;
        tiered
            .store("e1", "test content", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(tiered.stm.lock().await.count().await.unwrap(), 1);
        assert_eq!(tiered.mtm.lock().await.count().await.unwrap(), 0);
        assert_eq!(tiered.ltm.lock().await.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn get_checks_all_tiers() {
        let tiered = make_test_tiered().await;
        // Store in LTM directly
        tiered
            .ltm
            .lock()
            .await
            .store("ltm-entry", "in long term", MemoryCategory::Core, None)
            .await
            .unwrap();
        // get() should find it
        let result = tiered.get("ltm-entry").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().content, "in long term");
    }

    #[tokio::test]
    async fn get_prefers_stm_over_ltm() {
        let tiered = make_test_tiered().await;
        tiered
            .stm
            .lock()
            .await
            .store("shared", "stm version", MemoryCategory::Core, None)
            .await
            .unwrap();
        tiered
            .ltm
            .lock()
            .await
            .store("shared", "ltm version", MemoryCategory::Core, None)
            .await
            .unwrap();
        let result = tiered.get("shared").await.unwrap().unwrap();
        assert_eq!(result.content, "stm version");
    }

    #[tokio::test]
    async fn forget_removes_from_all_tiers() {
        let tiered = make_test_tiered().await;
        tiered
            .stm
            .lock()
            .await
            .store("del", "delete me", MemoryCategory::Core, None)
            .await
            .unwrap();
        tiered
            .mtm
            .lock()
            .await
            .store("del", "delete me", MemoryCategory::Core, None)
            .await
            .unwrap();
        let removed = tiered.forget("del").await.unwrap();
        assert!(removed);
        assert!(tiered.stm.lock().await.get("del").await.unwrap().is_none());
        assert!(tiered.mtm.lock().await.get("del").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn count_sums_all_tiers() {
        let tiered = make_test_tiered().await;
        tiered
            .stm
            .lock()
            .await
            .store("a", "1", MemoryCategory::Core, None)
            .await
            .unwrap();
        tiered
            .mtm
            .lock()
            .await
            .store("b", "2", MemoryCategory::Core, None)
            .await
            .unwrap();
        tiered
            .ltm
            .lock()
            .await
            .store("c", "3", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(tiered.count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn list_returns_stm_only() {
        let tiered = make_test_tiered().await;
        tiered
            .stm
            .lock()
            .await
            .store("stm-item", "visible", MemoryCategory::Core, None)
            .await
            .unwrap();
        tiered
            .ltm
            .lock()
            .await
            .store("ltm-item", "hidden from list", MemoryCategory::Core, None)
            .await
            .unwrap();
        let listed = tiered.list(None, None).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, "stm-item");
    }

    #[tokio::test]
    async fn health_check_passes_when_all_tiers_healthy() {
        let tiered = make_test_tiered().await;
        assert!(tiered.health_check().await);
    }

    #[tokio::test]
    async fn name_returns_tiered() {
        let tiered = make_test_tiered().await;
        assert_eq!(tiered.name(), "tiered");
    }

    #[tokio::test]
    async fn recall_returns_entries_from_multiple_tiers() {
        // Use 0.0 threshold so nothing gets filtered out
        let tiered = make_test_tiered_with_threshold(0.0).await;

        tiered
            .stm
            .lock()
            .await
            .store("s1", "alpha query", MemoryCategory::Core, None)
            .await
            .unwrap();
        tiered
            .ltm
            .lock()
            .await
            .store("l1", "beta query", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = tiered.recall("query", 10, None).await.unwrap();
        assert!(
            !results.is_empty(),
            "recall should return entries from tiers"
        );
    }

    #[tokio::test]
    async fn forget_nonexistent_key_returns_false() {
        let tiered = make_test_tiered().await;
        let removed = tiered.forget("nonexistent").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn recency_score_recent_is_high() {
        let now = Utc::now().to_rfc3339();
        let score = recency_score_from_timestamp(&now);
        assert!(
            score > 0.99,
            "recent timestamp should score ~1.0, got {score}"
        );
    }

    #[tokio::test]
    async fn recency_score_old_is_low() {
        let old = (Utc::now() - chrono::Duration::hours(25)).to_rfc3339();
        let score = recency_score_from_timestamp(&old);
        assert!(
            score < 0.01,
            "25h-old timestamp should score ~0.0, got {score}"
        );
    }

    #[tokio::test]
    async fn recency_score_invalid_timestamp_is_zero() {
        let score = recency_score_from_timestamp("not-a-timestamp");
        assert!(
            (score - 0.0).abs() < f32::EPSILON,
            "invalid timestamp should score 0.0"
        );
    }

    #[tokio::test]
    async fn store_enqueues_extraction_when_extractor_set() {
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let (ext_tx, mut ext_rx) = mpsc::channel(16);
        let tiered = TieredMemory {
            stm: make_shared(InMemoryBackend::new()),
            mtm: make_shared(InMemoryBackend::new()),
            ltm: make_shared(InMemoryBackend::new()),
            cfg: Arc::new(TierConfig::default()),
            cmd_tx,
            extraction_tx: Some(ext_tx),
        };

        tiered
            .store("msg:agent:123", "Hello world", MemoryCategory::Core, None)
            .await
            .unwrap();

        let req = ext_rx.try_recv().expect("should have received an ExtractionRequest");
        assert_eq!(req.content, "Hello world");
        assert_eq!(req.role, "agent");
        assert_eq!(req.key, "msg:agent:123");
    }

    #[tokio::test]
    async fn store_works_without_extractor() {
        let tiered = make_test_tiered().await;
        // Should succeed without panic even though extraction_tx is None
        tiered
            .store("msg:user:456", "test content", MemoryCategory::Core, None)
            .await
            .unwrap();
        assert_eq!(tiered.stm.lock().await.count().await.unwrap(), 1);
    }
}
