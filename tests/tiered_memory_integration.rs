//! Integration tests — full STM → MTM → LTM tiered-memory pipeline.
//!
//! These tests exercise the complete compression pipeline end-to-end using the
//! crate's public API with in-memory backends, a [`MockSummarizer`], and a
//! [`BasicTagExtractor`].

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use tokio::sync::{mpsc, Mutex};

use zeroclaw::agent::memory_loader::MemoryLoader;
use zeroclaw::memory::tiered::budget::estimate_tokens;
use zeroclaw::memory::tiered::extraction_worker::run_extraction_worker;
use zeroclaw::memory::tiered::extractor::{FactEntryDraft, MockFactExtractor};
use zeroclaw::memory::tiered::facts::{
    FactConfidence, FactEntry, FactStatus, SourceRole, SourceTurnRef, VolatilityClass,
};
use zeroclaw::memory::tiered::loader::TieredMemoryLoader;
use zeroclaw::memory::tiered::manager::TierManager;
use zeroclaw::memory::tiered::summarization::MockSummarizer;
use zeroclaw::memory::tiered::tagging::BasicTagExtractor;
use zeroclaw::memory::tiered::types::{IndexEntry, TierConfig};
use zeroclaw::memory::tiered::{SharedMemory, TieredMemory};
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};

// ── InMemoryBackend ───────────────────────────────────────────────────────────

/// Simple HashMap-backed Memory implementation for integration tests.
///
/// Supports an optional timestamp override so entries can be dated to a
/// specific day (required for `compress_day` to match entries).
struct InMemoryBackend {
    entries: std::sync::Mutex<HashMap<String, MemoryEntry>>,
    /// When set, `store()` uses this timestamp instead of `Utc::now()`.
    ts_override: std::sync::Mutex<Option<String>>,
}

impl InMemoryBackend {
    fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(HashMap::new()),
            ts_override: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl Memory for InMemoryBackend {
    fn name(&self) -> &str {
        "in-memory-integration"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let ts = self
            .ts_override
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let entry = MemoryEntry {
            id: key.to_string(),
            key: key.to_string(),
            content: content.to_string(),
            category,
            timestamp: ts,
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_shared(backend: InMemoryBackend) -> SharedMemory {
    Arc::new(Mutex::new(Box::new(backend)))
}

/// Category name used for cross-tier index entries stored in STM.
const STM_INDEX_CATEGORY: &str = "stm_index";

/// Minimal no-op Memory used as the `_memory` argument to `load_context`.
struct NoopMemory;

#[async_trait]
impl Memory for NoopMemory {
    fn name(&self) -> &str {
        "noop"
    }
    async fn store(
        &self,
        _: &str,
        _: &str,
        _: MemoryCategory,
        _: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recall(&self, _: &str, _: usize, _: Option<&str>) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
    async fn get(&self, _: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(None)
    }
    async fn list(
        &self,
        _: Option<&MemoryCategory>,
        _: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
    async fn forget(&self, _: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn count(&self) -> anyhow::Result<usize> {
        Ok(0)
    }
    async fn health_check(&self) -> bool {
        true
    }
}

// ── Test 1: Full pipeline STM → MTM → LTM ────────────────────────────────────

#[tokio::test]
async fn full_pipeline_stm_to_mtm_to_ltm() {
    // 1. Create 3 in-memory backends.
    let stm_backend = InMemoryBackend::new();
    let mtm_backend = InMemoryBackend::new();
    let ltm_backend = InMemoryBackend::new();

    // Pin all STM entries to today so compress_day(today) picks them up.
    let today = Utc::now().date_naive();
    let today_ts = today.and_hms_opt(12, 0, 0).unwrap().and_utc().to_rfc3339();

    // Set the timestamp override on the STM backend before wrapping.
    *stm_backend.ts_override.lock().unwrap() = Some(today_ts);

    // Store 3 entries directly on the STM backend (before wrapping).
    stm_backend
        .store(
            "e1",
            "Added auth middleware for JWT validation",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
    stm_backend
        .store(
            "e2",
            "Fixed auth token refresh bug in session handler",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
    stm_backend
        .store(
            "e3",
            "Deployed auth service to staging environment",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();

    // Clear the override (pipeline-created entries use Utc::now).
    *stm_backend.ts_override.lock().unwrap() = None;

    // 2. Wrap backends as SharedMemory.
    let stm = make_shared(stm_backend);
    let mtm = make_shared(mtm_backend);
    let ltm = make_shared(ltm_backend);

    // Verify 3 entries in STM before compression.
    assert_eq!(stm.lock().await.count().await.unwrap(), 3);

    // 3. Also create a TieredMemory for recall / Memory trait access.
    let (cmd_tx, _cmd_rx) = mpsc::channel(16);
    let mut cfg = TierConfig::default();
    cfg.min_relevance_threshold = 0.0; // allow all results in tests
    let cfg = Arc::new(cfg);

    let tiered = TieredMemory {
        stm: Arc::clone(&stm),
        mtm: Arc::clone(&mtm),
        ltm: Arc::clone(&ltm),
        cfg: Arc::clone(&cfg),
        cmd_tx: cmd_tx.clone(),
        extraction_tx: None,
    };

    // 4. Create a TierManager with MockSummarizer + BasicTagExtractor.
    let (_tx, rx) = mpsc::channel(16);
    let manager = TierManager {
        stm: Arc::clone(&stm),
        mtm: Arc::clone(&mtm),
        ltm: Arc::clone(&ltm),
        cfg: Arc::clone(&cfg),
        summarizer: Arc::new(MockSummarizer),
        tag_extractor: Arc::new(BasicTagExtractor::new()),
        fact_extractor: None,
        compression_guard: Arc::new(Mutex::new(())),
        job_journal: Arc::new(Mutex::new(Vec::new())),
        rx,
    };

    // 5. Call compress_day(today).
    manager.compress_day(today).await.unwrap();

    // 6. Verify post-compression state.

    // 6a. MTM should have 1 summary entry.
    let mtm_entries = mtm.lock().await.list(None, None).await.unwrap();
    assert_eq!(
        mtm_entries.len(),
        1,
        "MTM should have exactly 1 summary entry, got {}",
        mtm_entries.len()
    );
    let mtm_key = format!("mtm-{}", today.format("%Y-%m-%d"));
    let mtm_entry = mtm.lock().await.get(&mtm_key).await.unwrap();
    assert!(
        mtm_entry.is_some(),
        "MTM should contain entry with key '{mtm_key}'"
    );
    let mtm_content = mtm_entry.unwrap().content;
    assert!(
        mtm_content.contains("3 entries"),
        "MockSummarizer should produce 'Summary of 3 entries ...', got: {mtm_content}"
    );

    // 6b. LTM should have 3 archived entries.
    let ltm_entries = ltm.lock().await.list(None, None).await.unwrap();
    assert_eq!(
        ltm_entries.len(),
        3,
        "LTM should have 3 archived entries, got {}",
        ltm_entries.len()
    );

    // 6c. STM should have 0 raw entries, only index entries.
    let stm_all = stm.lock().await.list(None, None).await.unwrap();
    let stm_raw: Vec<_> = stm_all
        .iter()
        .filter(|e| e.category != MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()))
        .collect();
    assert_eq!(
        stm_raw.len(),
        0,
        "STM should have 0 raw entries after compression, got {}",
        stm_raw.len()
    );

    let stm_index: Vec<_> = stm_all
        .iter()
        .filter(|e| e.category == MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()))
        .collect();
    assert_eq!(
        stm_index.len(),
        1,
        "STM should have exactly 1 index entry, got {}",
        stm_index.len()
    );

    // 6d. Index entry should contain tags, MTM ref, and source entry IDs.
    let idx: IndexEntry =
        serde_json::from_str(&stm_index[0].content).expect("index entry should be valid JSON");
    assert!(
        !idx.tags.is_empty(),
        "Index entry should have at least 1 tag"
    );
    assert_eq!(
        idx.mtm_ref_id,
        Some(mtm_key.clone()),
        "Index entry should reference MTM key"
    );
    assert_eq!(
        idx.source_entry_ids.len(),
        3,
        "Index entry should reference 3 source STM entries"
    );
    assert!(idx.confidence > 0.0, "Index entry confidence should be > 0");

    // 7. Call recall("auth") on TieredMemory — should return results from across tiers.
    let recall_results = tiered.recall("auth", 10, None).await.unwrap();
    assert!(
        !recall_results.is_empty(),
        "recall('auth') should return results from the pipeline"
    );

    // 8. Load context via TieredMemoryLoader — should have index + MTM sections.
    let loader = TieredMemoryLoader::new(Arc::clone(&stm), Arc::clone(&mtm), Arc::clone(&cfg));

    let context = loader.load_context(&NoopMemory, "auth").await.unwrap();

    // After compression, STM has only index entries (no raw), so:
    // - "Memory Index" section should be present (from the index entry).
    // - "Medium-Term Memory" section should be present (from the MTM summary).
    assert!(
        context.contains("## Memory Index"),
        "Loaded context should contain Memory Index section.\nGot:\n{context}"
    );
    assert!(
        context.contains("## Medium-Term Memory (Recent Summaries)"),
        "Loaded context should contain MTM section.\nGot:\n{context}"
    );
}

// ── Test 2: MTM budget overflow compresses oldest to LTM ──────────────────────

#[tokio::test]
async fn mtm_budget_overflow_compresses_oldest_to_ltm() {
    // 1. Set a very low MTM token budget so overflow is easy to trigger.
    let mut cfg = TierConfig::default();
    cfg.mtm_token_budget = 100; // ~400 chars
    cfg.mtm_budget_hysteresis = 20;
    cfg.mtm_max_batch_days = 7;
    let cfg = Arc::new(cfg);

    let stm_backend = InMemoryBackend::new();
    let mtm_backend = InMemoryBackend::new();
    let ltm_backend = InMemoryBackend::new();

    // 2. Manually store several MTM entries that collectively exceed the budget.
    // Each entry is ~200 chars => ~50 tokens. 4 entries = ~200 tokens > 100 budget.
    let content_200 = "x".repeat(200);

    // Give entries timestamps on different days so overflow picks oldest first.
    let day1_ts = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .to_rfc3339();
    let day2_ts = NaiveDate::from_ymd_opt(2026, 1, 2)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .to_rfc3339();
    let day3_ts = NaiveDate::from_ymd_opt(2026, 1, 3)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .to_rfc3339();
    let day4_ts = NaiveDate::from_ymd_opt(2026, 1, 4)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .to_rfc3339();

    // Store with specific timestamps using the override.
    *mtm_backend.ts_override.lock().unwrap() = Some(day1_ts);
    mtm_backend
        .store("mtm-day1", &content_200, MemoryCategory::Daily, None)
        .await
        .unwrap();

    *mtm_backend.ts_override.lock().unwrap() = Some(day2_ts);
    mtm_backend
        .store("mtm-day2", &content_200, MemoryCategory::Daily, None)
        .await
        .unwrap();

    *mtm_backend.ts_override.lock().unwrap() = Some(day3_ts);
    mtm_backend
        .store("mtm-day3", &content_200, MemoryCategory::Daily, None)
        .await
        .unwrap();

    *mtm_backend.ts_override.lock().unwrap() = Some(day4_ts);
    mtm_backend
        .store("mtm-day4", &content_200, MemoryCategory::Daily, None)
        .await
        .unwrap();

    *mtm_backend.ts_override.lock().unwrap() = None;

    let stm = make_shared(stm_backend);
    let mtm = make_shared(mtm_backend);
    let ltm = make_shared(ltm_backend);

    // Verify initial state: 4 MTM entries, 0 LTM entries.
    assert_eq!(mtm.lock().await.count().await.unwrap(), 4);
    assert_eq!(ltm.lock().await.count().await.unwrap(), 0);

    // Calculate total tokens before budget check.
    let total_before: usize = {
        let mtm_guard = mtm.lock().await;
        let entries = mtm_guard.list(None, None).await.unwrap();
        entries.iter().map(|e| estimate_tokens(&e.content)).sum()
    };
    assert!(
        total_before > cfg.mtm_token_budget,
        "Total MTM tokens ({total_before}) should exceed budget ({})",
        cfg.mtm_token_budget
    );

    // 3. Create TierManager and call check_mtm_budget().
    let (_tx, rx) = mpsc::channel(16);
    let manager = TierManager {
        stm: Arc::clone(&stm),
        mtm: Arc::clone(&mtm),
        ltm: Arc::clone(&ltm),
        cfg: Arc::clone(&cfg),
        summarizer: Arc::new(MockSummarizer),
        tag_extractor: Arc::new(BasicTagExtractor::new()),
        fact_extractor: None,
        compression_guard: Arc::new(Mutex::new(())),
        job_journal: Arc::new(Mutex::new(Vec::new())),
        rx,
    };

    manager.check_mtm_budget().await.unwrap();

    // 4. Verify: some MTM entries were compressed into LTM.
    let ltm_count = ltm.lock().await.count().await.unwrap();
    assert!(
        ltm_count > 0,
        "LTM should have at least 1 compressed entry after budget overflow"
    );

    // Verify: oldest entries were removed from MTM.
    let mtm_remaining = mtm.lock().await.list(None, None).await.unwrap();
    let mtm_remaining_count = mtm_remaining.len();
    assert!(
        mtm_remaining_count < 4,
        "Some MTM entries should have been evicted, still have {mtm_remaining_count}"
    );

    // Verify: remaining MTM total is under budget (with hysteresis).
    let total_after: usize = mtm_remaining
        .iter()
        .map(|e| estimate_tokens(&e.content))
        .sum();
    assert!(
        total_after <= cfg.mtm_token_budget,
        "Remaining MTM tokens ({total_after}) should be at or under budget ({})",
        cfg.mtm_token_budget
    );

    // Verify: LTM compressed entry contains the MockSummarizer output.
    let ltm_entries = ltm.lock().await.list(None, None).await.unwrap();
    let compressed_entry = ltm_entries
        .iter()
        .find(|e| e.key.starts_with("ltm-compressed-"))
        .expect("LTM should contain a compressed entry with ltm-compressed- prefix");
    assert!(
        compressed_entry.content.contains("Compressed"),
        "Compressed entry should contain MockSummarizer output, got: {}",
        compressed_entry.content
    );
}

// ── Test 3: Idempotency — compress_day skips if already done ──────────────────

#[tokio::test]
async fn compress_day_is_idempotent() {
    let stm_backend = InMemoryBackend::new();
    let mtm_backend = InMemoryBackend::new();
    let ltm_backend = InMemoryBackend::new();

    let today = Utc::now().date_naive();
    let today_ts = today.and_hms_opt(12, 0, 0).unwrap().and_utc().to_rfc3339();

    *stm_backend.ts_override.lock().unwrap() = Some(today_ts);
    stm_backend
        .store("e1", "first entry", MemoryCategory::Daily, None)
        .await
        .unwrap();
    *stm_backend.ts_override.lock().unwrap() = None;

    let stm = make_shared(stm_backend);
    let mtm = make_shared(mtm_backend);
    let ltm = make_shared(ltm_backend);

    let cfg = Arc::new(TierConfig::default());
    let job_journal = Arc::new(Mutex::new(Vec::new()));

    let (_tx, rx) = mpsc::channel(16);
    let manager = TierManager {
        stm: Arc::clone(&stm),
        mtm: Arc::clone(&mtm),
        ltm: Arc::clone(&ltm),
        cfg: Arc::clone(&cfg),
        summarizer: Arc::new(MockSummarizer),
        tag_extractor: Arc::new(BasicTagExtractor::new()),
        fact_extractor: None,
        compression_guard: Arc::new(Mutex::new(())),
        job_journal: Arc::clone(&job_journal),
        rx,
    };

    // First compression should work.
    manager.compress_day(today).await.unwrap();
    assert_eq!(mtm.lock().await.count().await.unwrap(), 1);
    assert_eq!(ltm.lock().await.count().await.unwrap(), 1);

    // Second call with same day should be a no-op (idempotency).
    let mtm_count_before = mtm.lock().await.count().await.unwrap();
    let ltm_count_before = ltm.lock().await.count().await.unwrap();

    manager.compress_day(today).await.unwrap();

    assert_eq!(
        mtm.lock().await.count().await.unwrap(),
        mtm_count_before,
        "MTM count should not change on idempotent re-run"
    );
    assert_eq!(
        ltm.lock().await.count().await.unwrap(),
        ltm_count_before,
        "LTM count should not change on idempotent re-run"
    );

    // Journal should have exactly 1 succeeded job.
    let journal = job_journal.lock().await;
    let succeeded_count = journal
        .iter()
        .filter(|j| j.status == zeroclaw::memory::tiered::types::CompressionJobStatus::Succeeded)
        .count();
    assert_eq!(
        succeeded_count, 1,
        "Journal should have exactly 1 succeeded job, got {succeeded_count}"
    );
}

// ── Test 4: Fact extraction stores structured facts ────────────────────────

#[tokio::test]
async fn fact_extraction_stores_structured_facts() {
    // 1. Create InMemoryBackend instances for stm, mtm, ltm.
    let stm = make_shared(InMemoryBackend::new());
    let mtm = make_shared(InMemoryBackend::new());
    let ltm = make_shared(InMemoryBackend::new());

    // 2. Create TieredMemory with extraction_tx = Some(tx).
    let (cmd_tx, _cmd_rx) = mpsc::channel(16);
    let (extraction_tx, extraction_rx) = mpsc::channel(16);
    let mut cfg = TierConfig::default();
    cfg.min_relevance_threshold = 0.0;
    let cfg = Arc::new(cfg);

    let tiered = TieredMemory {
        stm: Arc::clone(&stm),
        mtm: Arc::clone(&mtm),
        ltm: Arc::clone(&ltm),
        cfg: Arc::clone(&cfg),
        cmd_tx,
        extraction_tx: Some(extraction_tx.clone()),
    };

    // 3. Create MockFactExtractor returning a known fact.
    let mock_draft = FactEntryDraft {
        category: "personal".to_string(),
        subject: "user".to_string(),
        attribute: "name".to_string(),
        value: "John".to_string(),
        context_narrative: "User said their name is John.".to_string(),
        confidence: "high".to_string(),
        related_facts: vec![],
        volatility_class: "stable".to_string(),
    };
    let extractor: Arc<dyn zeroclaw::memory::tiered::extractor::FactExtractor> =
        Arc::new(MockFactExtractor::new(vec![mock_draft]));

    // 4. Spawn extraction_worker in background.
    let stm_clone = Arc::clone(&stm);
    let worker_handle = tokio::spawn(async move {
        run_extraction_worker(
            extraction_rx,
            stm_clone,
            extractor,
            "conv-integration".to_string(),
        )
        .await;
    });

    // 5. Store a user message via TieredMemory::store().
    tiered
        .store(
            "msg:user:100",
            "My name is John",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

    // 6. Wait briefly for extraction to process.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // 7. Drop the extraction_tx to close the channel, wait for worker to finish.
    drop(extraction_tx);
    drop(tiered); // This drops the other extraction_tx clone inside TieredMemory
    worker_handle.await.unwrap();

    // 8. Check STM for Core entries with fact: prefix.
    let stm_guard = stm.lock().await;
    let fact_key = "fact:personal:user:name";
    let entry = stm_guard.get(fact_key).await.unwrap();

    // 9. Assert the fact entry exists with correct key and value.
    assert!(
        entry.is_some(),
        "expected fact to be stored under key `{fact_key}` in STM"
    );
    let entry = entry.unwrap();
    assert_eq!(entry.category, MemoryCategory::Core);
    assert!(entry.key.starts_with("fact:"));

    // Verify the stored content is a valid FactEntry with the correct value.
    let fact: FactEntry = serde_json::from_str(&entry.content)
        .expect("fact entry content should be valid FactEntry JSON");
    assert_eq!(fact.value, "John");
    assert_eq!(fact.subject, "user");
    assert_eq!(fact.attribute, "name");
    assert_eq!(fact.category, "personal");
    assert_eq!(fact.extracted_by_tier, "stm");
}

// ── Test 5: Fact-first recall boosts facts over raw ────────────────────────

#[tokio::test]
async fn fact_first_recall_boosts_facts_over_raw() {
    // 1. Create TieredMemory (no extraction queue needed).
    let stm = make_shared(InMemoryBackend::new());
    let mtm = make_shared(InMemoryBackend::new());
    let ltm = make_shared(InMemoryBackend::new());

    let (cmd_tx, _cmd_rx) = mpsc::channel(16);
    let mut cfg = TierConfig::default();
    cfg.min_relevance_threshold = 0.0; // allow all results in tests
    let cfg = Arc::new(cfg);

    let tiered = TieredMemory {
        stm: Arc::clone(&stm),
        mtm: Arc::clone(&mtm),
        ltm: Arc::clone(&ltm),
        cfg: Arc::clone(&cfg),
        cmd_tx,
        extraction_tx: None,
    };

    // 2. Store a raw conversation entry in STM.
    stm.lock()
        .await
        .store(
            "conv:user:001",
            "My name is John and I like programming",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

    // 3. Store a fact entry (as JSON FactEntry) in STM with MemoryCategory::Core.
    let fact = FactEntry {
        fact_id: "f-recall-001".to_string(),
        fact_key: "fact:personal:user:name".to_string(),
        category: "personal".to_string(),
        subject: "user".to_string(),
        attribute: "name".to_string(),
        value: "John".to_string(),
        context_narrative: "User said their name is John.".to_string(),
        source_turn: SourceTurnRef {
            conversation_id: "conv-test".to_string(),
            turn_index: 1,
            message_id: None,
            role: SourceRole::User,
            timestamp_unix_ms: 1_700_000_000_000,
        },
        confidence: FactConfidence::High,
        related_facts: vec![],
        extracted_by_tier: "stm".to_string(),
        extracted_at_unix_ms: 1_700_000_001_000,
        source_role: SourceRole::User,
        status: FactStatus::Active,
        revision: 1,
        supersedes_fact_id: None,
        tags: vec!["personal".to_string()],
        volatility_class: VolatilityClass::Stable,
        ttl_days: None,
        expires_at_unix_ms: None,
        last_verified_unix_ms: None,
    };
    let fact_json = serde_json::to_string(&fact).unwrap();
    stm.lock()
        .await
        .store(
            "fact:personal:user:name",
            &fact_json,
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

    // 4. Call recall() with a query matching both.
    let results = tiered.recall("John", 10, None).await.unwrap();

    // 5. Assert the fact entry appears in results (it should score higher due to fact boost).
    assert!(!results.is_empty(), "recall('John') should return results");

    // Find the fact entry in results.
    let fact_result = results.iter().find(|e| e.key.starts_with("fact:"));
    assert!(
        fact_result.is_some(),
        "fact entry should appear in recall results"
    );

    // If both appear, the fact should be ranked first (higher score).
    if results.len() >= 2 {
        assert!(
            results[0].key.starts_with("fact:"),
            "fact entry should be ranked first due to fact-first boost, but got key: {}",
            results[0].key
        );
    }
}

// ── Test 6: Loader includes Known Facts section ────────────────────────────

#[tokio::test]
async fn loader_includes_known_facts_section() {
    // 1. Create InMemoryBackend for stm and mtm.
    let stm_backend = InMemoryBackend::new();

    // 2. Store a FactEntry JSON in STM as Core with key "fact:personal:user:name".
    let fact = FactEntry {
        fact_id: "f-loader-001".to_string(),
        fact_key: "fact:personal:user:name".to_string(),
        category: "personal".to_string(),
        subject: "user".to_string(),
        attribute: "name".to_string(),
        value: "John".to_string(),
        context_narrative: "User introduced themselves as John.".to_string(),
        source_turn: SourceTurnRef {
            conversation_id: "conv-loader".to_string(),
            turn_index: 1,
            message_id: None,
            role: SourceRole::User,
            timestamp_unix_ms: 1_700_000_000_000,
        },
        confidence: FactConfidence::High,
        related_facts: vec![],
        extracted_by_tier: "stm".to_string(),
        extracted_at_unix_ms: 1_700_000_001_000,
        source_role: SourceRole::User,
        status: FactStatus::Active,
        revision: 1,
        supersedes_fact_id: None,
        tags: vec!["personal".to_string()],
        volatility_class: VolatilityClass::Stable,
        ttl_days: None,
        expires_at_unix_ms: None,
        last_verified_unix_ms: None,
    };
    let fact_json = serde_json::to_string(&fact).unwrap();
    stm_backend
        .store(
            "fact:personal:user:name",
            &fact_json,
            MemoryCategory::Core,
            None,
        )
        .await
        .unwrap();

    let stm = make_shared(stm_backend);
    let mtm = make_shared(InMemoryBackend::new());
    let cfg = Arc::new(TierConfig::default());

    // 3. Create TieredMemoryLoader.
    let loader = TieredMemoryLoader::new(Arc::clone(&stm), Arc::clone(&mtm), Arc::clone(&cfg));

    // 4. Call load_context().
    let context = loader
        .load_context(&NoopMemory, "test query")
        .await
        .unwrap();

    // 5. Assert output contains "Known Facts".
    assert!(
        context.contains("Known Facts"),
        "Loaded context should contain 'Known Facts' section.\nGot:\n{context}"
    );

    // 6. Assert output contains the fact value ("John").
    assert!(
        context.contains("John"),
        "Loaded context should contain the fact value 'John'.\nGot:\n{context}"
    );

    // Also verify confidence tag and subject/attribute are present.
    assert!(
        context.contains("[high]"),
        "Loaded context should contain confidence tag '[high]'.\nGot:\n{context}"
    );
    assert!(
        context.contains("user name"),
        "Loaded context should contain subject and attribute.\nGot:\n{context}"
    );
}
