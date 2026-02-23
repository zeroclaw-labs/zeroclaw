# Three-Tier Memory System Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Overhaul zeroclaw's flat memory system with a three-tier hierarchy (STM/MTM/LTM) that compresses short-term entries daily, enforces a token budget on medium-term, and archives raw details to long-term.

**Architecture:** A `TieredMemory` wrapper struct holds three independent `Memory` impls (one per tier) behind `Arc<tokio::sync::Mutex<...>>` so both the agent and the background `TierManager` task can share them safely. `TieredMemory` implements the existing `Memory` trait unchanged — it is a drop-in replacement. All compression is orchestrated by `TierManager` in a background tokio task.

**Tech Stack:** Rust, tokio (async runtime), chrono + chrono-tz (time/timezone), async-trait, serde/serde_json, anyhow (error handling). LLM client is the existing zeroclaw `CompletionClient` (already in the codebase).

---

## Before You Start

- Design doc: `docs/plans/2026-02-22-tiered-memory-design.md` — read it for full rationale
- All paths below are relative to the zeroclaw repo root (see Task 0 for setup)
- Run `cargo test` after every task before committing
- The existing `Memory` trait is in `src/memory/traits.rs`; `MemoryEntry` and `MemoryCategory` live there too — read that file before touching anything
- `async_trait` macro is used throughout; always annotate trait impls with `#[async_trait]`

---

## Task 0: Fork and clone the repo

**Files:**
- Working dir: `/home/admin/whiskey`

**Step 1: Clone zeroclaw into whiskey**

```bash
cd /home/admin/whiskey
git clone https://github.com/zeroclaw-labs/zeroclaw.git .
```

Expected: full zeroclaw source tree in `/home/admin/whiskey`

**Step 2: Verify it builds**

```bash
cargo build 2>&1 | tail -5
```

Expected: `Finished dev [unoptimized + debuginfo]` (warnings OK, no errors)

**Step 3: Run existing tests to establish baseline**

```bash
cargo test 2>&1 | tail -10
```

Expected: all tests pass. Note any pre-existing failures so you don't confuse them with regressions.

**Step 4: Create feature branch**

```bash
git checkout -b feature/tiered-memory
```

**Step 5: Commit baseline**

```bash
git add -A
git commit -m "chore: fork zeroclaw as base for tiered memory"
```

---

## Task 1: Define core types

**Files:**
- Create: `src/memory/tiered/types.rs`
- Create: `src/memory/tiered/mod.rs` (empty module shell)

**Context:** All shared structs and enums live here. Every other tiered module imports from here.

**Step 1: Write the failing test**

Create `src/memory/tiered/types.rs` with just the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn tier_config_defaults_are_sane() {
        let cfg = TierConfig::default();
        assert_eq!(cfg.stm_eod_hour, 23);
        assert_eq!(cfg.stm_eod_minute, 59);
        assert_eq!(cfg.mtm_token_budget, 2000);
        assert!(cfg.min_relevance_threshold > 0.0);
        assert_eq!(cfg.weight_stm + cfg.weight_mtm + cfg.weight_ltm, 1.0);
    }

    #[test]
    fn compression_job_initial_state_is_pending() {
        let job = CompressionJob::new_stm_to_mtm(chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
        assert_eq!(job.status, CompressionJobStatus::Pending);
        assert_eq!(job.attempts, 0);
        assert!(job.last_error.is_none());
    }

    #[test]
    fn index_entry_id_format() {
        let entry = IndexEntry {
            id: "idx-auth-2026-01-15".to_string(),
            topic: "auth".to_string(),
            tags: vec!["auth".to_string(), "middleware".to_string()],
            day: chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            created_at: chrono::Utc::now(),
            mtm_ref_id: Some("mtm-2026-01-15".to_string()),
            ltm_ref_id: Some("ltm-auth-2026-01-15".to_string()),
            source_entry_ids: vec!["stm-001".to_string()],
            confidence: 0.9,
            extractor_version: "1.0".to_string(),
        };
        assert!(entry.id.starts_with("idx-"));
        assert!(entry.tags.len() >= 2);
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test memory::tiered::types 2>&1 | tail -5
```

Expected: compile error — `TierConfig`, `CompressionJob`, `IndexEntry` not found

**Step 3: Write the types**

Replace `src/memory/tiered/types.rs` with full implementation:

```rust
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// ── TierConfig ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Hour of day (local time) to trigger STM→MTM compression (0–23)
    pub stm_eod_hour: u8,
    /// Minute of day to trigger STM→MTM compression (0–59)
    pub stm_eod_minute: u8,
    /// Max age for raw STM entries before they are eligible for compression
    #[serde(with = "humantime_serde")]
    pub stm_window: Duration,
    /// User's location string, e.g. "Cebu, Philippines"
    pub user_location: Option<String>,
    /// Resolved IANA timezone ID, e.g. "Asia/Manila"
    pub resolved_timezone: String,
    /// How the timezone was resolved: "user_location" | "system_fallback" | "manual"
    pub timezone_provenance: String,
    /// Max tokens MTM may contribute to a single prompt
    pub mtm_token_budget: usize,
    /// Buffer to avoid thrashing — only compress when overflow > hysteresis
    pub mtm_budget_hysteresis: usize,
    /// Max number of MTM day-summaries to batch-compress at once
    pub mtm_max_batch_days: usize,
    /// Max LLM call attempts per compression job
    pub compression_retry_max: u32,
    /// Initial backoff in seconds (doubles each retry)
    pub compression_retry_base_backoff_secs: u64,
    /// Max entries fetched from each tier during recall
    pub recall_top_k_per_tier: usize,
    /// Final number of entries returned from recall()
    pub recall_final_top_k: usize,
    /// Recall weight for STM results (should sum to 1.0 with mtm+ltm)
    pub weight_stm: f32,
    /// Recall weight for MTM results
    pub weight_mtm: f32,
    /// Recall weight for LTM results
    pub weight_ltm: f32,
    /// Minimum final score for a result to be included (matches existing loader 0.4)
    pub min_relevance_threshold: f32,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            stm_eod_hour: 23,
            stm_eod_minute: 59,
            stm_window: Duration::from_secs(24 * 3600),
            user_location: None,
            resolved_timezone: local_system_timezone(),
            timezone_provenance: "system_fallback".to_string(),
            mtm_token_budget: 2000,
            mtm_budget_hysteresis: 200,
            mtm_max_batch_days: 7,
            compression_retry_max: 3,
            compression_retry_base_backoff_secs: 30,
            recall_top_k_per_tier: 10,
            recall_final_top_k: 5,
            weight_stm: 0.45,
            weight_mtm: 0.35,
            weight_ltm: 0.20,
            min_relevance_threshold: 0.4,
        }
    }
}

/// Returns the system's local IANA timezone ID, e.g. "America/New_York".
/// Falls back to "UTC" if detection fails.
pub fn local_system_timezone() -> String {
    // iana_time_zone crate provides get_timezone(); add it to Cargo.toml if missing.
    // Fallback: use the TZ env var.
    std::env::var("TZ")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

// ── IndexEntry ────────────────────────────────────────────────────────────────

/// Stored in STM as MemoryCategory::Custom("stm_index").
/// Points to the MTM summary and/or LTM archive for a given topic/day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Format: "idx-{topic}-{YYYY-MM-DD}"
    pub id: String,
    /// Primary semantic grouping label, e.g. "auth"
    pub topic: String,
    /// 2–6 lowercase kebab-case semantic tags
    pub tags: Vec<String>,
    /// The calendar day these entries summarize
    pub day: NaiveDate,
    pub created_at: DateTime<Utc>,
    /// Reference to the MTM day-summary MemoryEntry id
    pub mtm_ref_id: Option<String>,
    /// Reference to the LTM archived MemoryEntry id (or comma-joined list)
    pub ltm_ref_id: Option<String>,
    /// IDs of the raw STM entries that were compressed to produce this index
    pub source_entry_ids: Vec<String>,
    /// Tag extraction confidence (0.0–1.0)
    pub confidence: f32,
    /// Version of the tag extractor used
    pub extractor_version: String,
}

impl IndexEntry {
    pub fn new(topic: &str, day: NaiveDate) -> Self {
        Self {
            id: format!("idx-{}-{}", topic, day),
            topic: topic.to_string(),
            tags: vec![],
            day,
            created_at: Utc::now(),
            mtm_ref_id: None,
            ltm_ref_id: None,
            source_entry_ids: vec![],
            confidence: 0.0,
            extractor_version: "1.0".to_string(),
        }
    }

    /// Serialize to a human-readable string for storage in MemoryEntry.content
    pub fn to_display(&self) -> String {
        format!(
            "[{}] {} → MTM:{}, LTM:{}",
            self.tags.join(", "),
            self.day,
            self.mtm_ref_id.as_deref().unwrap_or("none"),
            self.ltm_ref_id.as_deref().unwrap_or("none"),
        )
    }
}

// ── CompressionJob ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CompressionJobKind {
    StmDayToMtm { day: NaiveDate },
    MtmOverflowToLtm { mtm_ids: Vec<String> },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CompressionJobStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionJob {
    pub id: String,
    pub kind: CompressionJobKind,
    pub status: CompressionJobStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CompressionJob {
    pub fn new_stm_to_mtm(day: NaiveDate) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            kind: CompressionJobKind::StmDayToMtm { day },
            status: CompressionJobStatus::Pending,
            attempts: 0,
            last_error: None,
            next_retry_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn new_mtm_overflow(mtm_ids: Vec<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            kind: CompressionJobKind::MtmOverflowToLtm { mtm_ids },
            status: CompressionJobStatus::Pending,
            attempts: 0,
            last_error: None,
            next_retry_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.status, CompressionJobStatus::Succeeded | CompressionJobStatus::Failed)
    }
}

// ── TierCommand ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TierCommand {
    ForceEodCompression { day: NaiveDate },
    ForceMtmBudgetCheck,
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_config_defaults_are_sane() {
        let cfg = TierConfig::default();
        assert_eq!(cfg.stm_eod_hour, 23);
        assert_eq!(cfg.stm_eod_minute, 59);
        assert_eq!(cfg.mtm_token_budget, 2000);
        assert!(cfg.min_relevance_threshold > 0.0);
        // weights must sum to 1.0 (within float epsilon)
        let sum = cfg.weight_stm + cfg.weight_mtm + cfg.weight_ltm;
        assert!((sum - 1.0).abs() < 1e-5, "weights sum={}", sum);
    }

    #[test]
    fn compression_job_initial_state_is_pending() {
        let job = CompressionJob::new_stm_to_mtm(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        );
        assert_eq!(job.status, CompressionJobStatus::Pending);
        assert_eq!(job.attempts, 0);
        assert!(job.last_error.is_none());
    }

    #[test]
    fn index_entry_display_includes_refs() {
        let mut entry = IndexEntry::new("auth", chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
        entry.tags = vec!["auth".to_string(), "middleware".to_string()];
        entry.mtm_ref_id = Some("mtm-001".to_string());
        entry.ltm_ref_id = Some("ltm-001".to_string());
        let display = entry.to_display();
        assert!(display.contains("auth"));
        assert!(display.contains("mtm-001"));
        assert!(display.contains("ltm-001"));
    }
}
```

**Step 4: Create module shell**

```rust
// src/memory/tiered/mod.rs
pub mod types;
```

**Step 5: Run test**

```bash
cargo test memory::tiered::types 2>&1 | tail -10
```

Expected: all 3 tests pass

**Step 6: Commit**

```bash
git add src/memory/tiered/
git commit -m "feat(memory/tiered): define core types — TierConfig, IndexEntry, CompressionJob"
```

---

## Task 2: Token budget module

**Files:**
- Create: `src/memory/tiered/budget.rs`

**Context:** Determines how many tokens MTM content uses, and selects which MTM entries to compress when over budget. Uses the existing ~4 chars/token heuristic.

**Step 1: Write failing tests**

```rust
// src/memory/tiered/budget.rs — tests section only first
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_approx_4_chars_per_token() {
        let text = "a".repeat(400);
        let tokens = estimate_tokens(&text);
        assert_eq!(tokens, 100);
    }

    #[test]
    fn select_batch_picks_oldest_until_freed() {
        // Three MTM entries: 500, 300, 200 tokens. Budget=800, hysteresis=100.
        // Current total = 1000. Overflow = 1000 - 800 = 200. Need to free >= 200+100=300.
        // Should select oldest (500 tokens) → frees 500 >= 300. Done.
        let entries = vec![
            MtmEntry { id: "old".to_string(), token_count: 500, day: make_day(1) },
            MtmEntry { id: "mid".to_string(), token_count: 300, day: make_day(2) },
            MtmEntry { id: "new".to_string(), token_count: 200, day: make_day(3) },
        ];
        let batch = select_overflow_batch(&entries, 1000, 800, 100, 7);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, "old");
    }

    #[test]
    fn select_batch_picks_multiple_when_needed() {
        // Each entry is 150 tokens, total=450, budget=200, hysteresis=50. Need to free >=300.
        // One entry frees 150 < 300. Two free 300 >= 300. Should select oldest two.
        let entries = vec![
            MtmEntry { id: "a".to_string(), token_count: 150, day: make_day(1) },
            MtmEntry { id: "b".to_string(), token_count: 150, day: make_day(2) },
            MtmEntry { id: "c".to_string(), token_count: 150, day: make_day(3) },
        ];
        let batch = select_overflow_batch(&entries, 450, 200, 50, 7);
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].id, "a");
        assert_eq!(batch[1].id, "b");
    }

    #[test]
    fn no_overflow_returns_empty_batch() {
        let entries = vec![
            MtmEntry { id: "x".to_string(), token_count: 100, day: make_day(1) },
        ];
        let batch = select_overflow_batch(&entries, 100, 2000, 200, 7);
        assert!(batch.is_empty());
    }

    fn make_day(n: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 1, n).unwrap()
    }
}
```

**Step 2: Verify test fails**

```bash
cargo test memory::tiered::budget 2>&1 | tail -5
```

Expected: compile error — types not found

**Step 3: Implement**

```rust
use chrono::NaiveDate;

/// Estimate token count using ~4 chars per token heuristic.
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// A lightweight descriptor for an MTM entry used during batch selection.
#[derive(Debug, Clone)]
pub struct MtmEntry {
    pub id: String,
    pub token_count: usize,
    pub day: NaiveDate,
}

/// Returns the subset of MTM entries (oldest-first) that should be compressed
/// to bring the total under budget + hysteresis.
/// Returns empty vec if no overflow.
///
/// # Arguments
/// - `entries`: all current MTM entries, in any order (will be sorted internally)
/// - `current_total`: total tokens across all MTM entries
/// - `budget`: hard token cap
/// - `hysteresis`: buffer — only compress when overflow > hysteresis
/// - `max_batch_days`: cap on how many entries to select at once
pub fn select_overflow_batch(
    entries: &[MtmEntry],
    current_total: usize,
    budget: usize,
    hysteresis: usize,
    max_batch_days: usize,
) -> Vec<MtmEntry> {
    let target = budget.saturating_sub(hysteresis);
    if current_total <= target + hysteresis {
        return vec![];
    }
    let must_free = current_total.saturating_sub(target);

    let mut sorted: Vec<&MtmEntry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.day); // oldest first

    let mut batch = vec![];
    let mut freed = 0usize;

    for entry in sorted.into_iter().take(max_batch_days) {
        batch.push(entry.clone());
        freed += entry.token_count;
        if freed >= must_free {
            break;
        }
    }
    batch
}

#[cfg(test)]
mod tests { /* (tests from Step 1 above) */ }
```

**Step 4: Run tests**

```bash
cargo test memory::tiered::budget 2>&1 | tail -10
```

Expected: all 4 tests pass

**Step 5: Wire into tiered/mod.rs**

```rust
// src/memory/tiered/mod.rs
pub mod budget;
pub mod types;
```

**Step 6: Commit**

```bash
git add src/memory/tiered/budget.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): token budget — estimate_tokens + select_overflow_batch"
```

---

## Task 3: Recall merge module

**Files:**
- Create: `src/memory/tiered/merge.rs`

**Context:** After fetching candidates from all three tiers concurrently, this module merges, scores, deduplicates, and applies the diversity guard. Read the design doc §"Multi-Tier Recall Merging" before implementing.

**Step 1: Write failing tests**

```rust
// Tests only, add to bottom of merge.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tiered::types::MemoryTier;

    fn make_item(id: &str, tier: MemoryTier, base: f32, age_secs: i64) -> TieredRecallItem {
        TieredRecallItem {
            entry_id: id.to_string(),
            origin_id: id.to_string(), // same as entry_id for simple cases
            tier,
            base_score: base,
            recency_score: 1.0 - (age_secs as f32 / 86400.0).min(1.0),
            has_cross_tier_link: false,
            final_score: 0.0,
        }
    }

    #[test]
    fn merge_deduplicates_by_origin_id() {
        let items = vec![
            make_item("a", MemoryTier::Stm, 0.9, 100),
            make_item("a", MemoryTier::Ltm, 0.8, 1000), // same origin_id
            make_item("b", MemoryTier::Mtm, 0.7, 500),
        ];
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.4, 5);
        assert_eq!(merged.len(), 2, "should deduplicate 'a'");
        // STM version of 'a' should win (higher weight * base)
        assert_eq!(merged[0].tier, MemoryTier::Stm);
    }

    #[test]
    fn merge_applies_relevance_threshold() {
        let items = vec![
            make_item("high", MemoryTier::Stm, 0.9, 100),
            make_item("low",  MemoryTier::Ltm, 0.1, 100), // will score below 0.4
        ];
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.4, 5);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].entry_id, "high");
    }

    #[test]
    fn merge_respects_top_k() {
        let items: Vec<_> = (0..10)
            .map(|i| make_item(&i.to_string(), MemoryTier::Stm, 0.9 - i as f32 * 0.05, 100))
            .collect();
        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.0, 3);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn diversity_guard_includes_one_stm_and_one_mtm() {
        // 5 LTM items with high scores, 1 STM and 1 MTM with low scores.
        // Diversity guard should include at least one STM and one MTM.
        let mut items: Vec<_> = (0..5)
            .map(|i| make_item(&format!("ltm-{}", i), MemoryTier::Ltm, 0.95, 1000))
            .collect();
        items.push(make_item("stm-1", MemoryTier::Stm, 0.5, 50));
        items.push(make_item("mtm-1", MemoryTier::Mtm, 0.5, 200));

        let weights = TierWeights { stm: 0.45, mtm: 0.35, ltm: 0.20 };
        let merged = merge_and_rank(items, &weights, 0.0, 5);
        let has_stm = merged.iter().any(|i| i.tier == MemoryTier::Stm);
        let has_mtm = merged.iter().any(|i| i.tier == MemoryTier::Mtm);
        assert!(has_stm, "diversity guard must include at least one STM");
        assert!(has_mtm, "diversity guard must include at least one MTM");
    }
}
```

**Step 2: Verify test fails**

```bash
cargo test memory::tiered::merge 2>&1 | tail -5
```

**Step 3: Implement**

```rust
use crate::memory::tiered::types::MemoryTier;

#[derive(Debug, Clone, PartialEq)]
pub struct TieredRecallItem {
    pub entry_id: String,
    /// Root lineage ID for deduplication (compressed entries point to their source)
    pub origin_id: String,
    pub tier: MemoryTier,
    /// Normalized relevance score from the backend (0.0–1.0)
    pub base_score: f32,
    /// How recent the entry is, normalized to 0.0–1.0 (1.0 = just now)
    pub recency_score: f32,
    /// True if an STM IndexEntry explicitly references this MTM/LTM entry
    pub has_cross_tier_link: bool,
    /// Computed during merge
    pub final_score: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct TierWeights {
    pub stm: f32,
    pub mtm: f32,
    pub ltm: f32,
}

impl TierWeights {
    pub fn weight_for(&self, tier: MemoryTier) -> f32 {
        match tier {
            MemoryTier::Stm => self.stm,
            MemoryTier::Mtm => self.mtm,
            MemoryTier::Ltm => self.ltm,
        }
    }
}

/// Merge candidates from all tiers into a ranked, deduplicated list.
pub fn merge_and_rank(
    mut items: Vec<TieredRecallItem>,
    weights: &TierWeights,
    min_threshold: f32,
    top_k: usize,
) -> Vec<TieredRecallItem> {
    // 1. Score each item
    for item in items.iter_mut() {
        let tier_w = weights.weight_for(item.tier);
        let link_boost = if item.has_cross_tier_link { 0.15 } else { 0.0 };
        item.final_score = tier_w * item.base_score
            + 0.25 * item.recency_score
            + link_boost;
    }

    // 2. Deduplicate: keep highest final_score per origin_id
    items.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap());
    let mut seen_origins = std::collections::HashSet::new();
    let deduped: Vec<TieredRecallItem> = items
        .into_iter()
        .filter(|i| seen_origins.insert(i.origin_id.clone()))
        .collect();

    // 3. Apply threshold
    let above_threshold: Vec<TieredRecallItem> = deduped
        .into_iter()
        .filter(|i| i.final_score >= min_threshold)
        .collect();

    // 4. Diversity guard: reserve one slot each for STM and MTM if available
    apply_diversity_guard(above_threshold, top_k)
}

fn apply_diversity_guard(
    items: Vec<TieredRecallItem>,
    top_k: usize,
) -> Vec<TieredRecallItem> {
    if top_k == 0 {
        return vec![];
    }

    let stm_candidate = items.iter().find(|i| i.tier == MemoryTier::Stm).cloned();
    let mtm_candidate = items.iter().find(|i| i.tier == MemoryTier::Mtm).cloned();

    let reserved_ids: std::collections::HashSet<String> = [&stm_candidate, &mtm_candidate]
        .iter()
        .filter_map(|o| o.as_ref().map(|i| i.entry_id.clone()))
        .collect();

    let mut result: Vec<TieredRecallItem> = items
        .into_iter()
        .filter(|i| !reserved_ids.contains(&i.entry_id))
        .take(top_k.saturating_sub(reserved_ids.len()))
        .collect();

    if let Some(s) = stm_candidate { result.push(s); }
    if let Some(m) = mtm_candidate { result.push(m); }

    result.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap());
    result.truncate(top_k);
    result
}

#[cfg(test)]
mod tests { /* (tests from Step 1) */ }
```

**Step 4: Add `MemoryTier` to types.rs**

```rust
// Add to src/memory/tiered/types.rs:
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    Stm,
    Mtm,
    Ltm,
}
```

**Step 5: Run tests**

```bash
cargo test memory::tiered::merge 2>&1 | tail -10
```

Expected: all 4 tests pass

**Step 6: Update mod.rs and commit**

```bash
# Add `pub mod merge;` to src/memory/tiered/mod.rs
git add src/memory/tiered/
git commit -m "feat(memory/tiered): recall merge — weighted scoring, dedup, diversity guard"
```

---

## Task 4: TagExtractor trait and basic implementation

**Files:**
- Create: `src/memory/tiered/tagging.rs`

**Context:** Extracts semantic tags from a batch of MemoryEntry values. The "basic" implementation uses a simple keyword frequency approach — no LLM call needed. LLM-based refinement can be added later.

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_extractor_extracts_tags_from_content() {
        let extractor = BasicTagExtractor::new();
        let entries = vec![make_entry("auth middleware jwt token login session")];
        let (tags, confidence) = extractor.extract_tags(&entries).await.unwrap();
        assert!(!tags.is_empty());
        assert!(tags.len() <= 6);
        assert!(confidence > 0.0 && confidence <= 1.0);
        // Every tag is lowercase kebab-case
        for tag in &tags {
            assert!(tag.chars().all(|c| c.is_lowercase() || c == '-'));
        }
    }

    #[tokio::test]
    async fn basic_extractor_returns_at_least_two_tags_for_rich_content() {
        let extractor = BasicTagExtractor::new();
        let entries = vec![
            make_entry("database migration postgres schema"),
            make_entry("postgres connection pool timeout"),
        ];
        let (tags, _) = extractor.extract_tags(&entries).await.unwrap();
        assert!(tags.len() >= 2, "got {:?}", tags);
    }

    fn make_entry(content: &str) -> crate::memory::MemoryEntry {
        crate::memory::MemoryEntry {
            id: "test".to_string(),
            key: "test".to_string(),
            content: content.to_string(),
            category: crate::memory::MemoryCategory::Conversation,
            timestamp: chrono::Utc::now(),
            session_id: None,
            relevance_score: 1.0,
        }
    }
}
```

**Step 2: Verify test fails**

```bash
cargo test memory::tiered::tagging 2>&1 | tail -5
```

**Step 3: Implement**

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::memory::MemoryEntry;

/// Extracts semantic tags from a batch of memory entries.
#[async_trait]
pub trait TagExtractor: Send + Sync {
    /// Returns (tags, confidence). Tags: 2–6 lowercase kebab-case strings.
    async fn extract_tags(&self, entries: &[MemoryEntry]) -> Result<(Vec<String>, f32)>;
}

/// Simple frequency-based extractor. No LLM dependency.
pub struct BasicTagExtractor {
    /// Domain-specific important words to prioritize
    boost_words: Vec<String>,
    /// Common stop words to ignore
    stop_words: std::collections::HashSet<String>,
}

impl BasicTagExtractor {
    pub fn new() -> Self {
        Self {
            boost_words: vec![
                "auth", "database", "api", "cache", "error", "memory",
                "config", "test", "deploy", "migration", "session",
            ]
            .into_iter().map(str::to_string).collect(),
            stop_words: ["the", "a", "an", "is", "in", "on", "at", "to",
                         "and", "or", "of", "for", "with", "it", "was",
                         "i", "you", "we", "that", "this", "have", "be"]
                .iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Default for BasicTagExtractor {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl TagExtractor for BasicTagExtractor {
    async fn extract_tags(&self, entries: &[MemoryEntry]) -> Result<(Vec<String>, f32)> {
        use std::collections::HashMap;
        let mut freq: HashMap<String, usize> = HashMap::new();

        for entry in entries {
            for word in entry.content.split_whitespace() {
                let w = word
                    .trim_matches(|c: char| !c.is_alphabetic())
                    .to_lowercase();
                if w.len() < 3 || self.stop_words.contains(&w) {
                    continue;
                }
                let w = w.replace(' ', "-"); // kebab-case single words are unchanged
                *freq.entry(w).or_default() += 1;
            }
        }

        // Boost domain words
        for bw in &self.boost_words {
            if let Some(v) = freq.get_mut(bw) {
                *v = (*v).saturating_add(2);
            }
        }

        let mut sorted: Vec<(String, usize)> = freq.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));

        let tags: Vec<String> = sorted.into_iter()
            .map(|(w, _)| w)
            .take(6)
            .collect();

        let confidence = if tags.len() >= 2 { 0.7 } else { 0.3 };
        Ok((tags, confidence))
    }
}

#[cfg(test)]
mod tests { /* (tests from Step 1) */ }
```

**Step 4: Run tests**

```bash
cargo test memory::tiered::tagging 2>&1 | tail -10
```

Expected: both tests pass

**Step 5: Commit**

```bash
git add src/memory/tiered/tagging.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): TagExtractor trait + BasicTagExtractor"
```

---

## Task 5: TierSummarizer trait and LLM implementation

**Files:**
- Create: `src/memory/tiered/summarization.rs`

**Context:** Two methods — `summarize_stm_day()` (STM→MTM) and `compress_mtm_batch()` (MTM→LTM). Both call the existing zeroclaw LLM client. Before writing this task, read how the existing code calls the LLM (look for `CompletionClient` or similar in `src/`).

**Step 1: Write failing tests (using a mock summarizer)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryEntry;

    struct MockSummarizer;

    #[async_trait::async_trait]
    impl TierSummarizer for MockSummarizer {
        async fn summarize_stm_day(
            &self, _day: chrono::NaiveDate,
            entries: &[MemoryEntry], _tags: &[String],
        ) -> anyhow::Result<String> {
            Ok(format!("Summary of {} entries", entries.len()))
        }
        async fn compress_mtm_batch(&self, entries: &[MemoryEntry]) -> anyhow::Result<String> {
            Ok(format!("Compressed {} summaries", entries.len()))
        }
    }

    #[tokio::test]
    async fn mock_summarizer_returns_non_empty_string() {
        let s = MockSummarizer;
        let result = s.summarize_stm_day(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            &[],
            &["auth".to_string()],
        ).await.unwrap();
        assert!(!result.is_empty());
    }
}
```

**Step 2: Verify test fails**

```bash
cargo test memory::tiered::summarization 2>&1 | tail -5
```

**Step 3: Implement the trait**

```rust
use anyhow::Result;
use async_trait::async_trait;
use chrono::NaiveDate;
use crate::memory::MemoryEntry;

#[async_trait]
pub trait TierSummarizer: Send + Sync {
    /// Summarize a single day's STM entries into a compact MTM record.
    async fn summarize_stm_day(
        &self,
        day: NaiveDate,
        stm_entries: &[MemoryEntry],
        tags: &[String],
    ) -> Result<String>;

    /// Compress a batch of MTM day-summaries into a single LTM entry.
    async fn compress_mtm_batch(&self, mtm_entries: &[MemoryEntry]) -> Result<String>;
}

/// LLM-backed summarizer using the existing zeroclaw completion client.
pub struct LlmTierSummarizer {
    // Replace `CompletionClient` with the actual type used in zeroclaw.
    // Find it by running: grep -r "CompletionClient\|LlmClient\|completion" src/ | head -20
    pub client: std::sync::Arc<dyn crate::llm::CompletionClient>,
}

#[async_trait]
impl TierSummarizer for LlmTierSummarizer {
    async fn summarize_stm_day(
        &self,
        day: NaiveDate,
        stm_entries: &[MemoryEntry],
        tags: &[String],
    ) -> Result<String> {
        if stm_entries.is_empty() {
            return Ok(format!("No entries for {}", day));
        }
        let entries_text = stm_entries
            .iter()
            .map(|e| format!("- [{}] {}: {}", e.category, e.key, e.content))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Summarize the following memory entries from {} into a concise \
             paragraph (max 200 words). Topic tags: {}.\n\nEntries:\n{}",
            day,
            tags.join(", "),
            entries_text
        );

        // Adapt this call to match how the existing zeroclaw code calls the LLM.
        // Look for existing usages in src/agent/ or src/tools/.
        self.client.complete(&prompt).await
    }

    async fn compress_mtm_batch(&self, mtm_entries: &[MemoryEntry]) -> Result<String> {
        if mtm_entries.is_empty() {
            return Ok("No medium-term entries to compress.".to_string());
        }
        let summaries = mtm_entries
            .iter()
            .map(|e| format!("- {}: {}", e.key, e.content))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "Compress the following daily memory summaries into a single \
             long-term memory record (max 300 words). Preserve key facts, \
             decisions, and outcomes.\n\nSummaries:\n{}",
            summaries
        );

        self.client.complete(&prompt).await
    }
}

#[cfg(test)]
mod tests { /* (tests from Step 1) */ }
```

**Step 4: Run tests**

```bash
cargo test memory::tiered::summarization 2>&1 | tail -10
```

Expected: MockSummarizer test passes. LlmTierSummarizer compile errors are OK at this stage — fix them in Task 10 when wiring up.

**Step 5: Commit**

```bash
git add src/memory/tiered/summarization.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): TierSummarizer trait + LlmTierSummarizer"
```

---

## Task 6: TieredMemory struct — Memory trait implementation

**Files:**
- Modify: `src/memory/tiered/mod.rs`

**Context:** `TieredMemory` wraps three `Arc<tokio::sync::Mutex<Box<dyn Memory>>>` — one per tier. It implements the existing `Memory` trait. `store()` always writes to STM. `recall()` searches all three tiers concurrently and merges results. `forget()` removes from all tiers.

**Step 1: Understand the existing Memory trait signature**

Before writing code, run:
```bash
cat src/memory/traits.rs
```
Note the exact signatures (especially whether `store` takes `&mut self` or `&self`, and the exact `Result` type used).

**Step 2: Write failing tests**

```rust
// In src/memory/tiered/mod.rs, bottom of file:
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryEntry, MemoryCategory};

    fn make_entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            key: id.to_string(),
            content: content.to_string(),
            category: MemoryCategory::Conversation,
            timestamp: chrono::Utc::now(),
            session_id: None,
            relevance_score: 1.0,
        }
    }

    #[tokio::test]
    async fn store_writes_to_stm() {
        let tiered = TieredMemory::new_for_testing().await;
        let entry = make_entry("e1", "auth middleware test");
        tiered.store(entry).await.unwrap();
        let count = tiered.stm.lock().await.count().await.unwrap();
        assert_eq!(count, 1);
        let mtm_count = tiered.mtm.lock().await.count().await.unwrap();
        assert_eq!(mtm_count, 0, "store should only write to STM");
    }

    #[tokio::test]
    async fn recall_returns_relevant_entries() {
        let tiered = TieredMemory::new_for_testing().await;
        tiered.store(make_entry("e1", "auth middleware jwt")).await.unwrap();
        tiered.store(make_entry("e2", "database migration postgres")).await.unwrap();
        let results = tiered.recall("auth jwt", 5).await.unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn forget_removes_from_stm() {
        let tiered = TieredMemory::new_for_testing().await;
        tiered.store(make_entry("del-me", "to be deleted")).await.unwrap();
        tiered.forget("del-me").await.unwrap();
        let entry = tiered.stm.lock().await.get("del-me").await.unwrap();
        assert!(entry.is_none());
    }
}
```

**Step 3: Implement TieredMemory**

```rust
// src/memory/tiered/mod.rs
pub mod budget;
pub mod merge;
pub mod summarization;
pub mod tagging;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::memory::{Memory, MemoryEntry, MemoryCategory};
use crate::memory::tiered::merge::{TieredRecallItem, TierWeights, merge_and_rank};
use crate::memory::tiered::types::{MemoryTier, TierConfig, TierCommand};

/// Shared handle to a Memory backend — safe to clone across async tasks.
pub type SharedMemory = Arc<Mutex<Box<dyn Memory + Send>>>;

pub struct TieredMemory {
    pub stm: SharedMemory,
    pub mtm: SharedMemory,
    pub ltm: SharedMemory,
    pub cfg: Arc<TierConfig>,
    /// Send commands to the background TierManager task
    pub cmd_tx: mpsc::Sender<TierCommand>,
}

impl TieredMemory {
    pub fn new(
        stm: Box<dyn Memory + Send>,
        mtm: Box<dyn Memory + Send>,
        ltm: Box<dyn Memory + Send>,
        cfg: TierConfig,
        cmd_tx: mpsc::Sender<TierCommand>,
    ) -> Self {
        Self {
            stm: Arc::new(Mutex::new(stm)),
            mtm: Arc::new(Mutex::new(mtm)),
            ltm: Arc::new(Mutex::new(ltm)),
            cfg: Arc::new(cfg),
            cmd_tx,
        }
    }

    /// Convenience constructor for unit tests using in-memory backends.
    #[cfg(test)]
    pub async fn new_for_testing() -> Self {
        use crate::memory::none::NoneMemory; // or whatever the in-memory/test backend is
        let (tx, _rx) = mpsc::channel(16);
        Self::new(
            Box::new(NoneMemory::default()),
            Box::new(NoneMemory::default()),
            Box::new(NoneMemory::default()),
            TierConfig::default(),
            tx,
        )
    }

    /// Estimate token count of all current MTM entries.
    pub async fn mtm_token_count(&self) -> Result<usize> {
        use crate::memory::tiered::budget::estimate_tokens;
        let mtm = self.mtm.lock().await;
        let entries = mtm.list(None).await?;
        Ok(entries.iter().map(|e| estimate_tokens(&e.content)).sum())
    }
}

#[async_trait]
impl Memory for TieredMemory {
    async fn store(&mut self, entry: MemoryEntry) -> Result<()> {
        self.stm.lock().await.store(entry).await
    }

    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let k = self.cfg.recall_top_k_per_tier;

        // Fetch from all three tiers concurrently
        let (stm_res, mtm_res, ltm_res) = tokio::join!(
            async { self.stm.lock().await.recall(query, k).await },
            async { self.mtm.lock().await.recall(query, k).await },
            async { self.ltm.lock().await.recall(query, k).await },
        );

        let mut items: Vec<TieredRecallItem> = vec![];
        let now = chrono::Utc::now();

        fn to_tiered(entries: Vec<MemoryEntry>, tier: MemoryTier, now: chrono::DateTime<chrono::Utc>) -> Vec<TieredRecallItem> {
            entries.into_iter().map(|e| {
                let age_secs = (now - e.timestamp).num_seconds().max(0) as f32;
                TieredRecallItem {
                    origin_id: e.id.clone(),
                    entry_id: e.id.clone(),
                    tier,
                    base_score: e.relevance_score as f32,
                    recency_score: 1.0 - (age_secs / 86400.0).min(1.0),
                    has_cross_tier_link: false,
                    final_score: 0.0,
                }
            }).collect()
        }

        if let Ok(entries) = stm_res { items.extend(to_tiered(entries, MemoryTier::Stm, now)); }
        if let Ok(entries) = mtm_res { items.extend(to_tiered(entries, MemoryTier::Mtm, now)); }
        if let Ok(entries) = ltm_res { items.extend(to_tiered(entries, MemoryTier::Ltm, now)); }

        let weights = TierWeights {
            stm: self.cfg.weight_stm,
            mtm: self.cfg.weight_mtm,
            ltm: self.cfg.weight_ltm,
        };
        let ranked = merge_and_rank(items, &weights, self.cfg.min_relevance_threshold as f32, limit);

        // Re-fetch actual entries for ranked IDs (in order)
        // NOTE: In a real implementation, keep a HashMap<id, MemoryEntry> from the fetch above.
        // For now, return the STM entries directly (simplification — refine in integration test).
        let stm = self.stm.lock().await;
        let mut result = vec![];
        for item in ranked {
            if let Ok(Some(entry)) = stm.get(&item.entry_id).await {
                result.push(entry);
            }
        }
        Ok(result)
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        // Check STM first, then MTM, then LTM
        if let Some(e) = self.stm.lock().await.get(key).await? { return Ok(Some(e)); }
        if let Some(e) = self.mtm.lock().await.get(key).await? { return Ok(Some(e)); }
        self.ltm.lock().await.get(key).await
    }

    async fn list(&self, filter: Option<MemoryCategory>) -> Result<Vec<MemoryEntry>> {
        // Returns STM entries only (most recent, always injected)
        self.stm.lock().await.list(filter).await
    }

    async fn forget(&mut self, key: &str) -> Result<()> {
        let _ = self.stm.lock().await.forget(key).await;
        let _ = self.mtm.lock().await.forget(key).await;
        let _ = self.ltm.lock().await.forget(key).await;
        Ok(())
    }

    async fn count(&self) -> Result<usize> {
        let (a, b, c) = tokio::join!(
            async { self.stm.lock().await.count().await.unwrap_or(0) },
            async { self.mtm.lock().await.count().await.unwrap_or(0) },
            async { self.ltm.lock().await.count().await.unwrap_or(0) },
        );
        Ok(a + b + c)
    }

    async fn health_check(&self) -> Result<()> {
        self.stm.lock().await.health_check().await?;
        self.mtm.lock().await.health_check().await?;
        self.ltm.lock().await.health_check().await
    }
}
```

**Step 4: Run tests**

```bash
cargo test memory::tiered 2>&1 | tail -15
```

Expected: types, budget, merge, tagging, summarization, and new TieredMemory tests all pass

**Step 5: Commit**

```bash
git add src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): TieredMemory struct — Memory trait drop-in impl"
```

---

## Task 7: TierManager — compression pipeline and scheduler

**Files:**
- Create: `src/memory/tiered/manager.rs`

**Context:** Background tokio task. Runs the daily STM→MTM job at 23:59 local time and checks MTM budget after each compression. Uses `compression_guard: Mutex<()>` as the sole concurrency guard. All jobs are idempotent (two-phase commit with `CompressionJob` status).

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::tiered::summarization::TierSummarizer;
    use crate::memory::MemoryEntry;

    struct MockSummarizer;
    #[async_trait::async_trait]
    impl TierSummarizer for MockSummarizer {
        async fn summarize_stm_day(&self, day: chrono::NaiveDate, entries: &[MemoryEntry], _tags: &[String]) -> anyhow::Result<String> {
            Ok(format!("Summary of {} entries for {}", entries.len(), day))
        }
        async fn compress_mtm_batch(&self, entries: &[MemoryEntry]) -> anyhow::Result<String> {
            Ok(format!("Compressed {} summaries", entries.len()))
        }
    }

    #[tokio::test]
    async fn compress_day_moves_stm_to_mtm_and_ltm() {
        let (tiered, mut manager) = make_test_setup().await;
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // Store a couple of STM entries
        tiered.stm.lock().await.store(make_entry("e1", "auth middleware", day)).await.unwrap();
        tiered.stm.lock().await.store(make_entry("e2", "jwt token", day)).await.unwrap();

        manager.compress_day(day).await.unwrap();

        // STM should have 0 raw entries (they were archived)
        let stm_entries = tiered.stm.lock().await.list(None).await.unwrap();
        let raw: Vec<_> = stm_entries.iter()
            .filter(|e| e.category != crate::memory::MemoryCategory::Custom("stm_index".to_string()))
            .collect();
        assert_eq!(raw.len(), 0, "raw STM entries should be archived");

        // STM should have index entries
        let index_entries: Vec<_> = stm_entries.iter()
            .filter(|e| e.category == crate::memory::MemoryCategory::Custom("stm_index".to_string()))
            .collect();
        assert!(!index_entries.is_empty(), "should have index entries");

        // MTM should have a summary
        let mtm_count = tiered.mtm.lock().await.count().await.unwrap();
        assert_eq!(mtm_count, 1, "should have one MTM summary");

        // LTM should have archived entries
        let ltm_count = tiered.ltm.lock().await.count().await.unwrap();
        assert_eq!(ltm_count, 2, "should have 2 LTM archived entries");
    }

    #[tokio::test]
    async fn compress_day_is_idempotent() {
        let (tiered, mut manager) = make_test_setup().await;
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        tiered.stm.lock().await.store(make_entry("e1", "test entry", day)).await.unwrap();

        manager.compress_day(day).await.unwrap();
        manager.compress_day(day).await.unwrap(); // second call should be no-op

        let mtm_count = tiered.mtm.lock().await.count().await.unwrap();
        assert_eq!(mtm_count, 1, "idempotent: should still be 1 MTM entry");
    }

    fn make_entry(id: &str, content: &str, day: chrono::NaiveDate) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(), key: id.to_string(), content: content.to_string(),
            category: crate::memory::MemoryCategory::Conversation,
            timestamp: day.and_hms_opt(10, 0, 0).unwrap().and_utc(),
            session_id: None, relevance_score: 1.0,
        }
    }

    async fn make_test_setup() -> (Arc<crate::memory::tiered::TieredMemory>, TierManager) {
        // Returns a TieredMemory with in-memory backends and a TierManager using MockSummarizer
        todo!("wire up test setup after TierManager struct is defined")
    }
}
```

**Step 2: Verify test fails**

```bash
cargo test memory::tiered::manager 2>&1 | tail -5
```

**Step 3: Implement TierManager**

```rust
// src/memory/tiered/manager.rs
use anyhow::Result;
use chrono::{NaiveDate, Local, Timelike};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{sleep_until, Instant};

use crate::memory::{MemoryEntry, MemoryCategory};
use crate::memory::tiered::{SharedMemory, TieredMemory};
use crate::memory::tiered::budget::{estimate_tokens, select_overflow_batch, MtmEntry};
use crate::memory::tiered::summarization::TierSummarizer;
use crate::memory::tiered::tagging::TagExtractor;
use crate::memory::tiered::types::{
    TierCommand, TierConfig, CompressionJob, CompressionJobStatus, IndexEntry,
};

pub struct TierManager {
    pub stm: SharedMemory,
    pub mtm: SharedMemory,
    pub ltm: SharedMemory,
    pub cfg: Arc<TierConfig>,
    pub summarizer: Arc<dyn TierSummarizer>,
    pub tag_extractor: Arc<dyn TagExtractor>,
    /// Ensures only one compression job runs at a time
    pub compression_guard: Arc<Mutex<()>>,
    /// In-memory job journal (use a persistent store in production)
    pub job_journal: Arc<Mutex<Vec<CompressionJob>>>,
    pub rx: mpsc::Receiver<TierCommand>,
}

impl TierManager {
    /// Start the background event loop. Call this in a spawned tokio task.
    pub async fn run(mut self) {
        let mut next_eod = self.next_eod_instant();
        loop {
            tokio::select! {
                _ = sleep_until(next_eod) => {
                    let today = Local::now().date_naive();
                    if let Err(e) = self.compress_day(today).await {
                        tracing::error!("STM→MTM compression failed: {e}");
                    }
                    next_eod = self.next_eod_instant();
                }
                Some(cmd) = self.rx.recv() => {
                    match cmd {
                        TierCommand::ForceEodCompression { day } => {
                            let _ = self.compress_day(day).await;
                        }
                        TierCommand::ForceMtmBudgetCheck => {
                            let _ = self.check_mtm_budget().await;
                        }
                        TierCommand::Shutdown => break,
                    }
                }
            }
        }
    }

    /// Compute the next tokio Instant for today's (or tomorrow's) EOD trigger.
    fn next_eod_instant(&self) -> Instant {
        let now = Local::now();
        let target_h = self.cfg.stm_eod_hour as u32;
        let target_m = self.cfg.stm_eod_minute as u32;

        let mut target = now.date_naive()
            .and_hms_opt(target_h, target_m, 0)
            .expect("invalid EOD time")
            .and_local_timezone(Local)
            .unwrap();

        if target <= now {
            target = target + chrono::Duration::days(1);
        }
        let secs = (target - now).num_seconds().max(0) as u64;
        Instant::now() + std::time::Duration::from_secs(secs)
    }

    /// Compress all STM entries for `day` into MTM + archive to LTM.
    /// Idempotent: if a succeeded job exists for this day, returns Ok immediately.
    pub async fn compress_day(&mut self, day: NaiveDate) -> Result<()> {
        // Check if already succeeded
        {
            let journal = self.job_journal.lock().await;
            if journal.iter().any(|j| {
                matches!(&j.kind, crate::memory::tiered::types::CompressionJobKind::StmDayToMtm { day: d } if *d == day)
                && j.status == CompressionJobStatus::Succeeded
            }) {
                return Ok(());
            }
        }

        let _guard = self.compression_guard.lock().await;

        // 1. Snapshot STM entries for this day
        let stm_entries: Vec<MemoryEntry> = {
            let stm = self.stm.lock().await;
            stm.list(None).await?
                .into_iter()
                .filter(|e| {
                    let entry_day = e.timestamp.date_naive();
                    entry_day == day
                    && e.category != MemoryCategory::Custom("stm_index".to_string())
                })
                .collect()
        };

        if stm_entries.is_empty() {
            return Ok(());
        }

        // 2. Extract tags
        let (tags, confidence) = self.tag_extractor.extract_tags(&stm_entries).await
            .unwrap_or_else(|_| (vec!["general".to_string()], 0.5));

        // 3. LLM summarize → MTM entry
        let summary = self.summarizer.summarize_stm_day(day, &stm_entries, &tags).await?;

        let mtm_id = format!("mtm-{}", day);
        let mtm_entry = MemoryEntry {
            id: mtm_id.clone(),
            key: mtm_id.clone(),
            content: summary,
            category: MemoryCategory::Custom("mtm_summary".to_string()),
            timestamp: day.and_hms_opt(23, 59, 0).unwrap().and_utc(),
            session_id: None,
            relevance_score: 1.0,
        };
        self.mtm.lock().await.store(mtm_entry).await?;

        // 4. Archive raw STM entries → LTM
        let mut ltm_ids = vec![];
        for entry in &stm_entries {
            let ltm_entry = MemoryEntry {
                id: format!("ltm-{}", entry.id),
                ..entry.clone()
            };
            ltm_ids.push(ltm_entry.id.clone());
            self.ltm.lock().await.store(ltm_entry).await?;
        }

        // 5. Write IndexEntry to STM
        let topic = tags.first().cloned().unwrap_or_else(|| "general".to_string());
        let mut index = IndexEntry::new(&topic, day);
        index.tags = tags;
        index.confidence = confidence;
        index.mtm_ref_id = Some(mtm_id);
        index.ltm_ref_id = Some(ltm_ids.join(","));
        index.source_entry_ids = stm_entries.iter().map(|e| e.id.clone()).collect();

        let index_entry = MemoryEntry {
            id: index.id.clone(),
            key: index.id.clone(),
            content: index.to_display(),
            category: MemoryCategory::Custom("stm_index".to_string()),
            timestamp: chrono::Utc::now(),
            session_id: None,
            relevance_score: 1.0,
        };
        self.stm.lock().await.store(index_entry).await?;

        // 6. Delete raw STM entries
        let mut stm = self.stm.lock().await;
        for entry in &stm_entries {
            let _ = stm.forget(&entry.id).await;
        }
        drop(stm);

        // 7. Mark job succeeded in journal
        {
            let mut journal = self.job_journal.lock().await;
            let mut job = CompressionJob::new_stm_to_mtm(day);
            job.status = CompressionJobStatus::Succeeded;
            journal.push(job);
        }

        // 8. Check MTM budget
        self.check_mtm_budget().await?;
        Ok(())
    }

    /// If MTM total tokens exceeds budget, compress oldest batch → LTM.
    pub async fn check_mtm_budget(&mut self) -> Result<()> {
        let mtm_entries = self.mtm.lock().await.list(None).await?;
        let entries: Vec<MtmEntry> = mtm_entries.iter().map(|e| MtmEntry {
            id: e.id.clone(),
            token_count: estimate_tokens(&e.content),
            day: e.timestamp.date_naive(),
        }).collect();

        let total: usize = entries.iter().map(|e| e.token_count).sum();
        let batch = select_overflow_batch(
            &entries, total,
            self.cfg.mtm_token_budget,
            self.cfg.mtm_budget_hysteresis,
            self.cfg.mtm_max_batch_days,
        );

        if batch.is_empty() { return Ok(()); }

        let _guard = self.compression_guard.lock().await;
        let batch_entries: Vec<MemoryEntry> = mtm_entries.into_iter()
            .filter(|e| batch.iter().any(|b| b.id == e.id))
            .collect();

        let compressed = self.summarizer.compress_mtm_batch(&batch_entries).await?;
        let ltm_id = format!("ltm-compressed-{}", uuid::Uuid::new_v4());
        let ltm_entry = MemoryEntry {
            id: ltm_id.clone(),
            key: ltm_id.clone(),
            content: compressed,
            category: MemoryCategory::Core,
            timestamp: chrono::Utc::now(),
            session_id: None,
            relevance_score: 1.0,
        };
        self.ltm.lock().await.store(ltm_entry).await?;

        let mut mtm = self.mtm.lock().await;
        for b in &batch {
            let _ = mtm.forget(&b.id).await;
        }
        Ok(())
    }
}
```

**Step 4: Fill in the `make_test_setup()` function in tests and run**

```bash
cargo test memory::tiered::manager 2>&1 | tail -15
```

Expected: compress_day tests pass; idempotency test passes

**Step 5: Commit**

```bash
git add src/memory/tiered/manager.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): TierManager — daily compression + budget enforcement"
```

---

## Task 8: Timezone resolution via LLM

**Files:**
- Create: `src/memory/tiered/timezone.rs`

**Context:** Background task that resolves `user_location` → IANA timezone using the LLM. Non-blocking: system timezone is pre-set immediately, LLM resolution happens asynchronously with a 2-3s timeout.

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sanitize_strips_control_chars_and_truncates() {
        let input = "Cebu\x00, Philippines".to_string();
        let sanitized = sanitize_location(input);
        assert!(!sanitized.contains('\x00'));
        assert!(sanitized.len() <= 256);
    }

    #[tokio::test]
    async fn sanitize_caps_length() {
        let long_input = "a".repeat(500);
        let sanitized = sanitize_location(long_input);
        assert!(sanitized.len() <= 256);
    }

    #[tokio::test]
    async fn validate_accepts_valid_iana_format() {
        assert!(is_valid_iana_tz("Asia/Manila"));
        assert!(is_valid_iana_tz("America/New_York"));
        assert!(is_valid_iana_tz("Europe/London"));
    }

    #[tokio::test]
    async fn validate_rejects_garbage() {
        assert!(!is_valid_iana_tz("not-a-timezone"));
        assert!(!is_valid_iana_tz(""));
        assert!(!is_valid_iana_tz("UTC+8")); // not IANA format
    }
}
```

**Step 2: Implement**

```rust
// src/memory/tiered/timezone.rs
use anyhow::Result;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

/// Sanitize user-provided location string before sending to LLM.
pub fn sanitize_location(input: String) -> String {
    input
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Returns true if `tz` matches IANA format (Region/City or Region/Area/City)
/// and is a known timezone in chrono-tz.
pub fn is_valid_iana_tz(tz: &str) -> bool {
    use std::str::FromStr;
    // chrono_tz::Tz::from_str returns Ok only for known IANA names
    chrono_tz::Tz::from_str(tz).is_ok()
}

/// Resolve a user location string to an IANA timezone ID via LLM.
/// Returns None if resolution fails or times out.
pub async fn resolve_timezone_via_llm(
    location: &str,
    llm_client: Arc<dyn crate::llm::CompletionClient>,
) -> Option<String> {
    let sanitized = sanitize_location(location.to_string());
    if sanitized.is_empty() { return None; }

    let prompt = serde_json::json!({
        "task": "timezone_lookup",
        "location": sanitized,
        "instructions": "Return only the IANA timezone ID for this location. Examples: Asia/Manila, America/New_York, Europe/London. Return nothing else — no explanation, no punctuation."
    }).to_string();

    let result = timeout(
        Duration::from_secs(3),
        llm_client.complete(&prompt),
    ).await;

    match result {
        Ok(Ok(tz)) => {
            let tz = tz.trim().to_string();
            if is_valid_iana_tz(&tz) { Some(tz) } else { None }
        }
        _ => None,
    }
}

/// Spawn a background task to resolve timezone and update TierConfig.
/// Returns immediately; update happens asynchronously.
pub fn spawn_timezone_resolution(
    location: String,
    llm_client: Arc<dyn crate::llm::CompletionClient>,
    cfg: Arc<std::sync::RwLock<crate::memory::tiered::types::TierConfig>>,
) {
    tokio::spawn(async move {
        if let Some(tz) = resolve_timezone_via_llm(&location, llm_client).await {
            if let Ok(mut config) = cfg.write() {
                config.resolved_timezone = tz;
                config.timezone_provenance = "user_location".to_string();
            }
        }
    });
}

#[cfg(test)]
mod tests { /* tests from Step 1 */ }
```

**Step 3: Run tests**

```bash
cargo test memory::tiered::timezone 2>&1 | tail -10
```

Expected: sanitize + validate tests pass; LLM-dependent tests skipped/mocked

**Step 4: Commit**

```bash
git add src/memory/tiered/timezone.rs src/memory/tiered/mod.rs
git commit -m "feat(memory/tiered): LLM timezone resolution — sanitize, validate, async resolve"
```

---

## Task 9: TieredMemoryLoader — always inject STM + MTM under budget

**Files:**
- Modify: `src/agent/memory_loader.rs`

**Context:** Read the existing `DefaultMemoryLoader` implementation before modifying. The new `TieredMemoryLoader` should: (1) inject all STM entries + STM index entries unconditionally, (2) inject MTM summaries up to the token budget, (3) fall through to the existing loader logic for LTM.

**Step 1: Read the existing loader**

```bash
cat src/agent/memory_loader.rs
```

**Step 2: Write failing test**

```rust
// Add to src/agent/memory_loader.rs
#[cfg(test)]
mod tiered_loader_tests {
    use super::*;

    #[tokio::test]
    async fn tiered_loader_always_includes_stm_entries() {
        // Build a TieredMemory with some STM entries and test that
        // load_context() returns them even if they have low relevance scores
        todo!("implement after TieredMemoryLoader struct is written")
    }
}
```

**Step 3: Implement TieredMemoryLoader**

```rust
// Add to src/agent/memory_loader.rs

pub struct TieredMemoryLoader {
    pub cfg: Arc<crate::memory::tiered::types::TierConfig>,
}

#[async_trait]
impl MemoryLoader for TieredMemoryLoader {
    async fn load_context(
        &self,
        memory: &dyn Memory,
        user_message: &str,
    ) -> Result<String> {
        use crate::memory::tiered::budget::estimate_tokens;
        use crate::memory::MemoryCategory;

        // Get all STM entries (raw + index)
        let all_stm = memory.list(None).await.unwrap_or_default();
        let stm_raw: Vec<_> = all_stm.iter()
            .filter(|e| e.category != MemoryCategory::Custom("stm_index".to_string()))
            .collect();
        let stm_index: Vec<_> = all_stm.iter()
            .filter(|e| e.category == MemoryCategory::Custom("stm_index".to_string()))
            .collect();

        // Get MTM entries, sort oldest-first, inject under budget
        let mtm_entries = {
            // Access MTM tier — only works if memory is a TieredMemory.
            // For now, query via recall with a broad query.
            memory.recall(user_message, self.cfg.recall_top_k_per_tier).await
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.category == MemoryCategory::Custom("mtm_summary".to_string()))
                .collect::<Vec<_>>()
        };

        let mut lines = vec!["## Active Memory (Short-Term)".to_string()];
        for e in &stm_raw {
            lines.push(format!("- [{}] {}", e.key, e.content));
        }

        if !stm_index.is_empty() {
            lines.push("\n## Memory Index".to_string());
            for e in &stm_index {
                lines.push(format!("- {}", e.content));
            }
        }

        let mut mtm_token_used = 0usize;
        let mut mtm_lines = vec![];
        for e in &mtm_entries {
            let tokens = estimate_tokens(&e.content);
            if mtm_token_used + tokens > self.cfg.mtm_token_budget {
                break;
            }
            mtm_lines.push(format!("- [{}] {}", e.key, e.content));
            mtm_token_used += tokens;
        }

        if !mtm_lines.is_empty() {
            lines.push("\n## Medium-Term Memory (Recent Summaries)".to_string());
            lines.extend(mtm_lines);
        }

        Ok(lines.join("\n"))
    }
}
```

**Step 4: Run tests**

```bash
cargo test agent::memory_loader 2>&1 | tail -10
```

**Step 5: Commit**

```bash
git add src/agent/memory_loader.rs
git commit -m "feat(agent): TieredMemoryLoader — always injects STM + MTM under token budget"
```

---

## Task 10: Wire up factory function and exports

**Files:**
- Modify: `src/memory/backend.rs`
- Modify: `src/memory/mod.rs`
- Modify: `Cargo.toml` (if `chrono-tz` or `uuid` or `humantime-serde` not present)

**Step 1: Check Cargo.toml for missing deps**

```bash
grep -E "chrono-tz|uuid|humantime" Cargo.toml
```

Add any missing deps. Example:
```toml
[dependencies]
chrono-tz = "0.9"
uuid = { version = "1", features = ["v4"] }
humantime-serde = "1"
```

**Step 2: Add export in mod.rs**

```rust
// src/memory/mod.rs — add:
pub mod tiered;
pub use tiered::TieredMemory;
```

**Step 3: Add factory function to backend.rs**

```rust
// src/memory/backend.rs — add:

/// Create a tiered memory instance with three independent SQLite backends.
/// STM and MTM use separate database files; LTM uses the existing backend.
pub async fn create_tiered_memory(
    data_dir: &std::path::Path,
    cfg: crate::memory::tiered::types::TierConfig,
    ltm: Box<dyn Memory + Send>,
) -> anyhow::Result<(crate::memory::tiered::TieredMemory, tokio::sync::mpsc::Sender<crate::memory::tiered::types::TierCommand>)> {
    let stm = Box::new(crate::memory::sqlite::SqliteMemory::new(
        &data_dir.join("stm.db")
    ).await?);
    let mtm = Box::new(crate::memory::sqlite::SqliteMemory::new(
        &data_dir.join("mtm.db")
    ).await?);

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let tiered = crate::memory::tiered::TieredMemory::new(stm, mtm, ltm, cfg, tx.clone());
    Ok((tiered, tx))
}
```

**Step 4: Run full test suite**

```bash
cargo test 2>&1 | tail -20
```

Expected: all existing tests still pass; new tiered tests pass

**Step 5: Commit**

```bash
git add src/memory/mod.rs src/memory/backend.rs Cargo.toml Cargo.lock
git commit -m "feat(memory): wire up TieredMemory factory + exports"
```

---

## Task 11: Integration test — full pipeline

**Files:**
- Create: `tests/tiered_memory_integration.rs`

**Context:** End-to-end test: store entries in STM, trigger compression, verify IndexEntries created, verify MTM summary, verify LTM archive, verify recall returns merged results.

**Step 1: Write integration test**

```rust
// tests/tiered_memory_integration.rs
use zeroclaw::memory::tiered::{TieredMemory, manager::TierManager, types::TierConfig};
use zeroclaw::memory::{MemoryEntry, MemoryCategory, Memory};
use chrono::{NaiveDate, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

struct MockSummarizer;
#[async_trait::async_trait]
impl zeroclaw::memory::tiered::summarization::TierSummarizer for MockSummarizer {
    async fn summarize_stm_day(&self, day: NaiveDate, entries: &[MemoryEntry], _tags: &[String]) -> anyhow::Result<String> {
        Ok(format!("Daily summary for {}: {} entries processed", day, entries.len()))
    }
    async fn compress_mtm_batch(&self, entries: &[MemoryEntry]) -> anyhow::Result<String> {
        Ok(format!("Long-term compression: {} summaries", entries.len()))
    }
}

fn make_entry(id: &str, content: &str, day: NaiveDate) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(), key: id.to_string(), content: content.to_string(),
        category: MemoryCategory::Conversation,
        timestamp: day.and_hms_opt(10, 30, 0).unwrap().and_utc(),
        session_id: None, relevance_score: 1.0,
    }
}

#[tokio::test]
async fn full_pipeline_stm_to_mtm_to_ltm() {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let cfg = TierConfig::default();

    // Use in-memory backends for testing (NoneMemory or SqliteMemory with :memory:)
    let (stm, mtm, ltm) = make_test_backends().await;

    let tiered = TieredMemory {
        stm: stm.clone(), mtm: mtm.clone(), ltm: ltm.clone(),
        cfg: Arc::new(cfg.clone()), cmd_tx: tx,
    };

    // Store entries
    let day = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
    stm.lock().await.store(make_entry("e1", "debugged auth middleware", day)).await.unwrap();
    stm.lock().await.store(make_entry("e2", "fixed jwt token refresh", day)).await.unwrap();
    stm.lock().await.store(make_entry("e3", "deployed to production", day)).await.unwrap();

    // Trigger compression
    let mut manager = TierManager {
        stm: stm.clone(), mtm: mtm.clone(), ltm: ltm.clone(),
        cfg: Arc::new(cfg),
        summarizer: Arc::new(MockSummarizer),
        tag_extractor: Arc::new(zeroclaw::memory::tiered::tagging::BasicTagExtractor::new()),
        compression_guard: Arc::new(Mutex::new(())),
        job_journal: Arc::new(Mutex::new(vec![])),
        rx,
    };
    manager.compress_day(day).await.unwrap();

    // Verify MTM has a summary
    let mtm_entries = mtm.lock().await.list(None).await.unwrap();
    assert_eq!(mtm_entries.len(), 1, "should have 1 MTM summary");
    assert!(mtm_entries[0].content.contains("3 entries processed"));

    // Verify LTM has all 3 archived entries
    let ltm_entries = ltm.lock().await.list(None).await.unwrap();
    assert_eq!(ltm_entries.len(), 3, "should have 3 LTM archived entries");

    // Verify STM has only index entries (no raw entries)
    let stm_entries = stm.lock().await.list(None).await.unwrap();
    let raw: Vec<_> = stm_entries.iter()
        .filter(|e| e.category != MemoryCategory::Custom("stm_index".to_string()))
        .collect();
    assert!(raw.is_empty(), "raw STM entries should be gone");
    let index_entries: Vec<_> = stm_entries.iter()
        .filter(|e| e.category == MemoryCategory::Custom("stm_index".to_string()))
        .collect();
    assert!(!index_entries.is_empty(), "index entries should exist");

    // Verify index entry contains tags and refs
    let idx = &index_entries[0];
    assert!(idx.content.contains("MTM:"), "index should reference MTM: {}", idx.content);
    assert!(idx.content.contains("LTM:"), "index should reference LTM: {}", idx.content);
}

async fn make_test_backends() -> (
    zeroclaw::memory::tiered::SharedMemory,
    zeroclaw::memory::tiered::SharedMemory,
    zeroclaw::memory::tiered::SharedMemory,
) {
    // Replace with actual in-memory backend — e.g. SqliteMemory::new(":memory:")
    use zeroclaw::memory::none::NoneMemory;
    (
        Arc::new(Mutex::new(Box::new(NoneMemory::default()) as Box<dyn zeroclaw::memory::Memory + Send>)),
        Arc::new(Mutex::new(Box::new(NoneMemory::default()) as Box<dyn zeroclaw::memory::Memory + Send>)),
        Arc::new(Mutex::new(Box::new(NoneMemory::default()) as Box<dyn zeroclaw::memory::Memory + Send>)),
    )
}
```

**Step 2: Run integration test**

```bash
cargo test tiered_memory_integration 2>&1 | tail -20
```

Expected: all assertions pass

**Step 3: Run full test suite — no regressions**

```bash
cargo test 2>&1 | grep -E "test result|FAILED|error"
```

Expected: `test result: ok. N passed; 0 failed`

**Step 4: Final commit**

```bash
git add tests/tiered_memory_integration.rs
git commit -m "test(memory/tiered): integration test — full STM→MTM→LTM pipeline"
```

---

## Completion Checklist

Before declaring done:

- [ ] `cargo test` passes with 0 failures
- [ ] `cargo clippy -- -D warnings` passes
- [ ] STM entries always appear in prompt injection
- [ ] Daily compression job fires at correct local time
- [ ] MTM token budget is enforced
- [ ] All compression jobs are idempotent
- [ ] Existing memory backend tests still pass (no regression)
- [ ] `health_check()` returns degraded when compression jobs fail
