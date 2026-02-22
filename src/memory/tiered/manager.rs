//! TierManager — daily STM→MTM compression pipeline and MTM budget enforcement.
//!
//! The [`TierManager`] is the background coordinator that:
//! 1. Runs an end-of-day timer to compress raw STM entries into MTM summaries.
//! 2. Listens for [`TierCommand`]s (force compression, budget check, shutdown).
//! 3. Enforces the MTM token budget by spilling oldest summaries to LTM.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::memory::tiered::budget::{estimate_tokens, select_overflow_batch, MtmEntry};
use crate::memory::tiered::extractor::FactExtractor;
use crate::memory::tiered::facts::*;
use crate::memory::tiered::prompts::build_mtm_extraction_prompt;
use crate::memory::tiered::summarization::TierSummarizer;
use crate::memory::tiered::tagging::TagExtractor;
use crate::memory::tiered::types::{
    CompressionJob, CompressionJobKind, CompressionJobStatus, IndexEntry, TierCommand, TierConfig,
};
use crate::memory::tiered::SharedMemory;
use crate::memory::traits::{MemoryCategory, MemoryEntry};

// ── TierManager ───────────────────────────────────────────────────────────────

/// Background coordinator for the tiered-memory compression pipeline.
pub struct TierManager {
    pub stm: SharedMemory,
    pub mtm: SharedMemory,
    pub ltm: SharedMemory,
    pub cfg: Arc<TierConfig>,
    pub summarizer: Arc<dyn TierSummarizer>,
    pub tag_extractor: Arc<dyn TagExtractor>,
    pub fact_extractor: Option<Arc<dyn FactExtractor>>,
    pub compression_guard: Arc<Mutex<()>>,
    pub job_journal: Arc<Mutex<Vec<CompressionJob>>>,
    pub rx: mpsc::Receiver<TierCommand>,
}

/// Category name used for cross-tier index entries stored in STM.
const STM_INDEX_CATEGORY: &str = "stm_index";

impl TierManager {
    /// Main event loop.
    ///
    /// Uses `tokio::select!` to wait for either the EOD timer or a command
    /// from the channel. Runs until a [`TierCommand::Shutdown`] is received.
    pub async fn run(mut self) -> Result<()> {
        loop {
            let eod_sleep = self.duration_until_eod();

            tokio::select! {
                _ = tokio::time::sleep(eod_sleep) => {
                    let today = Utc::now().date_naive();
                    if let Err(e) = self.compress_day(today).await {
                        tracing::error!("EOD compression failed for {today}: {e:#}");
                    }
                }
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(TierCommand::ForceEodCompression { day }) => {
                            if let Err(e) = self.compress_day(day).await {
                                tracing::error!("Forced compression failed for {day}: {e:#}");
                            }
                        }
                        Some(TierCommand::ForceMtmBudgetCheck) => {
                            if let Err(e) = self.check_mtm_budget().await {
                                tracing::error!("MTM budget check failed: {e:#}");
                            }
                        }
                        Some(TierCommand::Shutdown) | None => {
                            tracing::info!("TierManager shutting down");
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Calculate the duration until the configured end-of-day time.
    fn duration_until_eod(&self) -> tokio::time::Duration {
        let tz_str = &self.cfg.resolved_timezone;
        let now_utc = Utc::now();

        // Try to parse as a chrono-tz timezone; fall back to UTC.
        let target_utc = tz_str
            .parse::<chrono_tz::Tz>()
            .ok()
            .and_then(|tz| {
                use chrono::TimeZone;
                let now_local = now_utc.with_timezone(&tz);
                let today = now_local.date_naive();
                let eod = today
                    .and_hms_opt(self.cfg.stm_eod_hour as u32, self.cfg.stm_eod_minute as u32, 0)?;
                let eod_local = tz.from_local_datetime(&eod).single()?;
                let eod_utc = eod_local.with_timezone(&Utc);
                if eod_utc <= now_utc {
                    // Already past today's EOD — target tomorrow.
                    let tomorrow = today.succ_opt()?;
                    let eod_tomorrow = tomorrow.and_hms_opt(
                        self.cfg.stm_eod_hour as u32,
                        self.cfg.stm_eod_minute as u32,
                        0,
                    )?;
                    let eod_tomorrow_local = tz.from_local_datetime(&eod_tomorrow).single()?;
                    Some(eod_tomorrow_local.with_timezone(&Utc))
                } else {
                    Some(eod_utc)
                }
            });

        match target_utc {
            Some(target) => {
                let delta = target.signed_duration_since(now_utc);
                let secs = delta.num_seconds().max(1) as u64;
                tokio::time::Duration::from_secs(secs)
            }
            None => {
                // Fallback: 1 hour.
                tokio::time::Duration::from_secs(3600)
            }
        }
    }

    // ── compress_day ──────────────────────────────────────────────────────

    /// Core compression pipeline: STM → MTM + LTM archival.
    ///
    /// Steps:
    /// 1. Idempotency check via job journal.
    /// 2. Acquire compression guard.
    /// 3. Snapshot STM entries for the given day (excluding index entries).
    /// 4. Extract tags.
    /// 5. Summarize via LLM (or mock) → store MTM entry.
    /// 6. Archive raw entries to LTM.
    /// 7. Create IndexEntry, store as STM index.
    /// 8. Delete raw STM entries.
    /// 9. Mark job as succeeded.
    /// 10. Run MTM budget check.
    pub async fn compress_day(&self, day: NaiveDate) -> Result<()> {
        // (a) Idempotency: skip if already succeeded for this day.
        {
            let journal = self.job_journal.lock().await;
            let already_done = journal.iter().any(|j| {
                j.status == CompressionJobStatus::Succeeded
                    && matches!(&j.kind, CompressionJobKind::StmDayToMtm { day: d } if *d == day)
            });
            if already_done {
                return Ok(());
            }
        }

        // (b) Acquire compression guard.
        let _guard = self.compression_guard.lock().await;

        // (c) Snapshot: get all STM entries for this day, excluding stm_index.
        let stm_entries = {
            let stm = self.stm.lock().await;
            let all = stm.list(None, None).await?;
            all.into_iter()
                .filter(|e| {
                    e.category != MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string())
                        && entry_matches_day(e, day)
                })
                .collect::<Vec<_>>()
        };

        // (d) If no entries, nothing to compress.
        if stm_entries.is_empty() {
            return Ok(());
        }

        // (e) Extract tags.
        let (tags, confidence) = self.tag_extractor.extract_tags(&stm_entries).await?;

        // (f) Summarize → create MTM entry.
        let summary_text = self
            .summarizer
            .summarize_stm_day(day, &stm_entries, &tags)
            .await
            .context("STM day summarization failed")?;

        let mtm_key = format!("mtm-{}", day.format("%Y-%m-%d"));
        {
            let mtm = self.mtm.lock().await;
            mtm.store(&mtm_key, &summary_text, MemoryCategory::Daily, None)
                .await
                .context("storing MTM summary")?;
        }

        // (f.1) Deep fact extraction via MTM agent (if available).
        if let Some(ref extractor) = self.fact_extractor {
            let transcript = stm_entries.iter()
                .map(|e| format!("[{}] {}: {}", e.timestamp, e.key, e.content))
                .collect::<Vec<_>>()
                .join("\n");

            // Gather existing facts for cross-referencing.
            let existing_facts: Vec<String> = {
                let stm_guard = self.stm.lock().await;
                stm_guard.list(Some(&MemoryCategory::Core), None).await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|e| e.key.starts_with("fact:"))
                    .map(|e| format!("{} → {}", e.key, e.content))
                    .collect()
            };
            let fact_refs: Vec<&str> = existing_facts.iter().map(|s| s.as_str()).collect();
            let prompt = build_mtm_extraction_prompt(&transcript, &fact_refs);

            match extractor.extract(&prompt).await {
                Ok(drafts) => {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    let stm_guard = self.stm.lock().await;
                    for draft in drafts {
                        let fact_key = build_fact_key(&draft.category, &draft.subject, &draft.attribute);
                        let confidence = match draft.confidence.to_lowercase().as_str() {
                            "high" => FactConfidence::High,
                            "low" => FactConfidence::Low,
                            _ => FactConfidence::Medium,
                        };
                        let volatility = match draft.volatility_class.to_lowercase().as_str() {
                            "stable" => VolatilityClass::Stable,
                            "volatile" => VolatilityClass::Volatile,
                            _ => VolatilityClass::SemiStable,
                        };
                        let entry = FactEntry {
                            fact_id: uuid::Uuid::new_v4().to_string(),
                            fact_key: fact_key.clone(),
                            category: draft.category,
                            subject: draft.subject,
                            attribute: draft.attribute,
                            value: draft.value,
                            context_narrative: draft.context_narrative,
                            source_turn: SourceTurnRef {
                                conversation_id: String::new(),
                                turn_index: 0,
                                message_id: None,
                                role: SourceRole::System,
                                timestamp_unix_ms: now_ms,
                            },
                            confidence,
                            related_facts: draft.related_facts,
                            extracted_by_tier: "mtm".to_string(),
                            extracted_at_unix_ms: now_ms,
                            source_role: SourceRole::System,
                            status: FactStatus::Active,
                            revision: 1,
                            supersedes_fact_id: None,
                            tags: tags.clone(),
                            volatility_class: volatility,
                            ttl_days: None,
                            expires_at_unix_ms: None,
                            last_verified_unix_ms: Some(now_ms),
                        };
                        let content = serde_json::to_string(&entry).unwrap_or_default();
                        let _ = stm_guard.store(&fact_key, &content, MemoryCategory::Core, None).await;
                    }
                }
                Err(e) => {
                    eprintln!("MTM deep fact extraction failed (non-fatal): {:#}", e);
                }
            }
        }

        // (g) Archive raw STM entries → store each in LTM.
        let source_ids: Vec<String> = stm_entries.iter().map(|e| e.id.clone()).collect();
        {
            let ltm = self.ltm.lock().await;
            for entry in &stm_entries {
                let ltm_key = format!("ltm-archive-{}", entry.id);
                ltm.store(&ltm_key, &entry.content, MemoryCategory::Daily, None)
                    .await
                    .context("archiving STM entry to LTM")?;
            }
        }

        // (h) Create IndexEntry, store in STM as stm_index category.
        let topic = tags.first().cloned().unwrap_or_else(|| "general".to_string());
        let mut idx = IndexEntry::new(&topic, day);
        idx.tags = tags;
        idx.mtm_ref_id = Some(mtm_key.clone());
        idx.source_entry_ids = source_ids.clone();
        idx.confidence = confidence;

        let idx_json =
            serde_json::to_string(&idx).context("serializing IndexEntry")?;
        {
            let stm = self.stm.lock().await;
            stm.store(
                &idx.id,
                &idx_json,
                MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()),
                None,
            )
            .await
            .context("storing index entry in STM")?;
        }

        // (i) Delete raw STM entries from STM.
        {
            let stm = self.stm.lock().await;
            for id in &source_ids {
                stm.forget(id).await.context("deleting raw STM entry")?;
            }
        }

        // (j) Mark job as succeeded in journal.
        {
            let mut journal = self.job_journal.lock().await;
            let mut job = CompressionJob::new_stm_to_mtm(day);
            job.status = CompressionJobStatus::Succeeded;
            job.attempts = 1;
            job.updated_at = Utc::now();
            journal.push(job);
        }

        // (k) Check MTM budget.
        self.check_mtm_budget().await?;

        Ok(())
    }

    // ── check_mtm_budget ──────────────────────────────────────────────────

    /// Budget enforcement: compress oldest MTM entries to LTM when over budget.
    pub async fn check_mtm_budget(&self) -> Result<()> {
        // (a) List all MTM entries.
        let mtm_entries = {
            let mtm = self.mtm.lock().await;
            mtm.list(None, None).await?
        };

        // (b) Calculate token counts.
        let mtm_descriptors: Vec<MtmEntry> = mtm_entries
            .iter()
            .map(|e| MtmEntry {
                id: e.id.clone(),
                token_count: estimate_tokens(&e.content),
                day: parse_entry_day(e),
            })
            .collect();

        let current_total: usize = mtm_descriptors.iter().map(|e| e.token_count).sum();

        // (c) Select overflow batch.
        let batch = select_overflow_batch(
            &mtm_descriptors,
            current_total,
            self.cfg.mtm_token_budget,
            self.cfg.mtm_budget_hysteresis,
            self.cfg.mtm_max_batch_days,
        );

        // (d) If batch is empty, nothing to do.
        if batch.is_empty() {
            return Ok(());
        }

        // Collect the actual MTM MemoryEntry values for the batch.
        let batch_ids: Vec<String> = batch.iter().map(|b| b.id.clone()).collect();
        let batch_entries: Vec<MemoryEntry> = mtm_entries
            .into_iter()
            .filter(|e| batch_ids.contains(&e.id))
            .collect();

        // (e) Compress batch via summarizer.
        let compressed_text = self
            .summarizer
            .compress_mtm_batch(&batch_entries)
            .await
            .context("MTM batch compression failed")?;

        // (f) Store compressed entry in LTM.
        let ltm_key = format!("ltm-compressed-{}", Uuid::new_v4());
        {
            let ltm = self.ltm.lock().await;
            ltm.store(&ltm_key, &compressed_text, MemoryCategory::Core, None)
                .await
                .context("storing compressed MTM batch in LTM")?;
        }

        // (g) Delete batch entries from MTM.
        {
            let mtm = self.mtm.lock().await;
            for id in &batch_ids {
                mtm.forget(id).await.context("deleting overflowed MTM entry")?;
            }
        }

        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Check whether a MemoryEntry's timestamp falls on the given day.
fn entry_matches_day(entry: &MemoryEntry, day: NaiveDate) -> bool {
    // Try RFC-3339 first, then NaiveDateTime.
    let parsed = entry
        .timestamp
        .parse::<chrono::DateTime<Utc>>()
        .ok()
        .map(|dt| dt.date_naive())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(&entry.timestamp, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|ndt| ndt.date())
        });

    parsed.map_or(false, |d| d == day)
}

/// Extract a NaiveDate from a MemoryEntry timestamp, defaulting to epoch.
fn parse_entry_day(entry: &MemoryEntry) -> NaiveDate {
    entry
        .timestamp
        .parse::<chrono::DateTime<Utc>>()
        .ok()
        .map(|dt| dt.date_naive())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(&entry.timestamp, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|ndt| ndt.date())
        })
        .unwrap_or(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
    use async_trait::async_trait;
    use chrono::{NaiveDate, Utc};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    // ── InMemoryBackend ───────────────────────────────────────────────────

    /// Simple HashMap-backed Memory implementation for testing.
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

    // ── TimestampInMemoryBackend ──────────────────────────────────────────

    /// Like InMemoryBackend but lets callers set a specific timestamp at store
    /// time via a thread-local override.
    struct TimestampInMemoryBackend {
        entries: std::sync::Mutex<HashMap<String, MemoryEntry>>,
    }

    thread_local! {
        static OVERRIDE_TIMESTAMP: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }

    impl TimestampInMemoryBackend {
        fn new() -> Self {
            Self {
                entries: std::sync::Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl Memory for TimestampInMemoryBackend {
        fn name(&self) -> &str {
            "timestamp-in-memory-test"
        }

        async fn store(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            let ts = OVERRIDE_TIMESTAMP.with(|cell| {
                cell.borrow().clone().unwrap_or_else(|| Utc::now().to_rfc3339())
            });
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

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_shared(backend: impl Memory + Send + 'static) -> SharedMemory {
        Arc::new(Mutex::new(Box::new(backend)))
    }

    /// Build a TierManager for tests. Returns (manager, stm, mtm, ltm) so
    /// tests can inspect tier contents after operations.
    async fn make_test_manager() -> (TierManager, SharedMemory, SharedMemory, SharedMemory) {
        use crate::memory::tiered::summarization::MockSummarizer;
        use crate::memory::tiered::tagging::BasicTagExtractor;

        let stm = make_shared(TimestampInMemoryBackend::new());
        let mtm = make_shared(InMemoryBackend::new());
        let ltm = make_shared(InMemoryBackend::new());
        let (_tx, rx) = mpsc::channel(16);

        let manager = TierManager {
            stm: Arc::clone(&stm),
            mtm: Arc::clone(&mtm),
            ltm: Arc::clone(&ltm),
            cfg: Arc::new(TierConfig::default()),
            summarizer: Arc::new(MockSummarizer),
            tag_extractor: Arc::new(BasicTagExtractor::new()),
            fact_extractor: None,
            compression_guard: Arc::new(Mutex::new(())),
            job_journal: Arc::new(Mutex::new(Vec::new())),
            rx,
        };

        (manager, stm, mtm, ltm)
    }

    /// Store a test entry in STM with a timestamp matching the given day.
    async fn store_test_entry(stm: &SharedMemory, key: &str, content: &str, day: NaiveDate) {
        let ts = day
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .to_rfc3339();

        OVERRIDE_TIMESTAMP.with(|cell| {
            *cell.borrow_mut() = Some(ts);
        });

        stm.lock()
            .await
            .store(key, content, MemoryCategory::Daily, None)
            .await
            .unwrap();

        OVERRIDE_TIMESTAMP.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }

    /// Store a test entry directly in MTM (for budget tests).
    #[allow(unused_variables)]
    async fn store_mtm_entry(mtm: &SharedMemory, key: &str, content: &str, day_num: u32) {
        mtm.lock()
            .await
            .store(key, content, MemoryCategory::Daily, None)
            .await
            .unwrap();
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn compress_day_moves_stm_to_mtm_and_ltm() {
        let (manager, stm, mtm, ltm) = make_test_manager().await;
        let day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // Store 2 STM entries for this day.
        store_test_entry(&stm, "e1", "auth middleware", day).await;
        store_test_entry(&stm, "e2", "jwt token", day).await;

        manager.compress_day(day).await.unwrap();

        // MTM should have 1 summary.
        let mtm_count = mtm.lock().await.count().await.unwrap();
        assert_eq!(mtm_count, 1, "MTM should have exactly 1 summary");

        // LTM should have 2 archived entries.
        let ltm_count = ltm.lock().await.count().await.unwrap();
        assert_eq!(ltm_count, 2, "LTM should have 2 archived raw entries");

        // STM should have only index entries (raw entries deleted).
        let stm_entries = stm.lock().await.list(None, None).await.unwrap();
        assert!(
            !stm_entries.is_empty(),
            "STM should still have index entries"
        );
        for entry in &stm_entries {
            assert_eq!(
                entry.category,
                MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string()),
                "all remaining STM entries should be index entries, found: {:?}",
                entry.category
            );
        }
    }

    #[tokio::test]
    async fn compress_day_is_idempotent() {
        let (manager, stm, mtm, _ltm) = make_test_manager().await;
        let day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        store_test_entry(&stm, "e1", "test", day).await;

        manager.compress_day(day).await.unwrap();
        manager.compress_day(day).await.unwrap(); // second call is no-op

        assert_eq!(
            mtm.lock().await.count().await.unwrap(),
            1,
            "should still be 1 after second compress_day"
        );
    }

    #[tokio::test]
    async fn compress_day_empty_is_noop() {
        let (manager, _stm, mtm, _ltm) = make_test_manager().await;
        let day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        manager.compress_day(day).await.unwrap();
        assert_eq!(
            mtm.lock().await.count().await.unwrap(),
            0,
            "empty day should produce no MTM entries"
        );
    }

    #[tokio::test]
    async fn check_mtm_budget_compresses_when_over_budget() {
        let (manager, _stm, mtm, ltm) = make_test_manager().await;

        // config default mtm_token_budget = 2000 tokens = ~8000 chars.
        // Store 2 entries that together exceed the budget.
        let big_content = "x".repeat(5000); // ~1250 tokens each
        store_mtm_entry(&mtm, "mtm-1", &big_content, 1).await;
        store_mtm_entry(&mtm, "mtm-2", &big_content, 2).await; // total ~2500 > 2000

        manager.check_mtm_budget().await.unwrap();

        // Oldest entry should have been compressed to LTM.
        assert!(
            ltm.lock().await.count().await.unwrap() > 0,
            "LTM should have at least one compressed entry"
        );
    }

    #[tokio::test]
    async fn check_mtm_budget_noop_when_under_budget() {
        let (manager, _stm, mtm, ltm) = make_test_manager().await;

        // Store a small entry well under budget.
        store_mtm_entry(&mtm, "mtm-small", "short text", 1).await;

        manager.check_mtm_budget().await.unwrap();

        // LTM should be empty — nothing to compress.
        assert_eq!(
            ltm.lock().await.count().await.unwrap(),
            0,
            "LTM should be empty when MTM is under budget"
        );
        // MTM should still have its entry.
        assert_eq!(
            mtm.lock().await.count().await.unwrap(),
            1,
            "MTM entry should not be removed when under budget"
        );
    }

    #[tokio::test]
    async fn compress_day_does_not_touch_index_entries() {
        let (manager, stm, mtm, _ltm) = make_test_manager().await;
        let day1 = NaiveDate::from_ymd_opt(2026, 1, 14).unwrap();
        let day2 = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // Compress day1 first to create an index entry.
        store_test_entry(&stm, "e1", "work on day1", day1).await;
        manager.compress_day(day1).await.unwrap();

        // Now compress day2 — should not remove day1's index entry.
        store_test_entry(&stm, "e2", "work on day2", day2).await;
        manager.compress_day(day2).await.unwrap();

        // STM should have 2 index entries (one for each day).
        let stm_entries = stm.lock().await.list(None, None).await.unwrap();
        let index_count = stm_entries
            .iter()
            .filter(|e| {
                e.category == MemoryCategory::Custom(STM_INDEX_CATEGORY.to_string())
            })
            .count();
        assert_eq!(index_count, 2, "should have 2 index entries");

        // MTM should have 2 summaries.
        assert_eq!(mtm.lock().await.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn run_handles_shutdown_command() {
        use crate::memory::tiered::summarization::MockSummarizer;
        use crate::memory::tiered::tagging::BasicTagExtractor;

        let stm = make_shared(InMemoryBackend::new());
        let mtm = make_shared(InMemoryBackend::new());
        let ltm = make_shared(InMemoryBackend::new());
        let (tx, rx) = mpsc::channel(16);

        let manager = TierManager {
            stm,
            mtm,
            ltm,
            cfg: Arc::new(TierConfig::default()),
            summarizer: Arc::new(MockSummarizer),
            tag_extractor: Arc::new(BasicTagExtractor::new()),
            fact_extractor: None,
            compression_guard: Arc::new(Mutex::new(())),
            job_journal: Arc::new(Mutex::new(Vec::new())),
            rx,
        };

        // Send Shutdown immediately.
        tx.send(TierCommand::Shutdown).await.unwrap();

        // run() should exit cleanly.
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            manager.run(),
        )
        .await;

        assert!(result.is_ok(), "run() should complete within timeout");
        assert!(result.unwrap().is_ok(), "run() should return Ok");
    }

    #[tokio::test]
    async fn run_handles_force_compression_command() {
        use crate::memory::tiered::summarization::MockSummarizer;
        use crate::memory::tiered::tagging::BasicTagExtractor;

        let stm = make_shared(TimestampInMemoryBackend::new());
        let mtm_backend = make_shared(InMemoryBackend::new());
        let ltm_backend = make_shared(InMemoryBackend::new());
        let (tx, rx) = mpsc::channel(16);

        let stm_clone = Arc::clone(&stm);
        let mtm_clone = Arc::clone(&mtm_backend);

        let manager = TierManager {
            stm,
            mtm: mtm_backend,
            ltm: ltm_backend,
            cfg: Arc::new(TierConfig::default()),
            summarizer: Arc::new(MockSummarizer),
            tag_extractor: Arc::new(BasicTagExtractor::new()),
            fact_extractor: None,
            compression_guard: Arc::new(Mutex::new(())),
            job_journal: Arc::new(Mutex::new(Vec::new())),
            rx,
        };

        let day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        store_test_entry(&stm_clone, "e1", "test entry", day).await;

        // Send ForceEodCompression then Shutdown.
        tx.send(TierCommand::ForceEodCompression { day }).await.unwrap();
        tx.send(TierCommand::Shutdown).await.unwrap();

        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            manager.run(),
        )
        .await;

        assert!(result.is_ok(), "run() should complete within timeout");
        assert!(result.unwrap().is_ok(), "run() should return Ok");

        // MTM should have 1 summary from the forced compression.
        assert_eq!(mtm_clone.lock().await.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn compress_day_extracts_facts_when_extractor_present() {
        use crate::memory::tiered::extractor::{FactEntryDraft, MockFactExtractor};
        use crate::memory::tiered::summarization::MockSummarizer;
        use crate::memory::tiered::tagging::BasicTagExtractor;

        let stm = make_shared(TimestampInMemoryBackend::new());
        let mtm = make_shared(InMemoryBackend::new());
        let ltm = make_shared(InMemoryBackend::new());
        let (_tx, rx) = mpsc::channel(16);

        let mock_drafts = vec![FactEntryDraft {
            category: "personal".to_string(),
            subject: "user".to_string(),
            attribute: "favorite_color".to_string(),
            value: "blue".to_string(),
            context_narrative: "User mentioned they love blue.".to_string(),
            confidence: "high".to_string(),
            related_facts: vec![],
            volatility_class: "stable".to_string(),
        }];

        let stm_clone = Arc::clone(&stm);

        let manager = TierManager {
            stm: Arc::clone(&stm),
            mtm: Arc::clone(&mtm),
            ltm: Arc::clone(&ltm),
            cfg: Arc::new(TierConfig::default()),
            summarizer: Arc::new(MockSummarizer),
            tag_extractor: Arc::new(BasicTagExtractor::new()),
            fact_extractor: Some(Arc::new(MockFactExtractor::new(mock_drafts))),
            compression_guard: Arc::new(Mutex::new(())),
            job_journal: Arc::new(Mutex::new(Vec::new())),
            rx,
        };

        let day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        store_test_entry(&stm_clone, "e1", "my favorite color is blue", day).await;

        manager.compress_day(day).await.unwrap();

        // Verify facts are stored in STM as Core entries.
        let stm_entries = stm_clone.lock().await.list(Some(&MemoryCategory::Core), None).await.unwrap();
        let fact_entries: Vec<_> = stm_entries.iter().filter(|e| e.key.starts_with("fact:")).collect();
        assert_eq!(fact_entries.len(), 1, "should have exactly 1 fact entry");
        assert_eq!(
            fact_entries[0].key, "fact:personal:user:favorite-color",
            "fact key should follow taxonomy"
        );

        // Verify the stored content is valid JSON FactEntry.
        let parsed: crate::memory::tiered::facts::FactEntry =
            serde_json::from_str(&fact_entries[0].content).expect("should parse as FactEntry");
        assert_eq!(parsed.value, "blue");
        assert_eq!(parsed.extracted_by_tier, "mtm");
        assert_eq!(parsed.confidence, crate::memory::tiered::facts::FactConfidence::High);
    }
}
