# Three-Tier Memory System Design

**Task ID:** 20260222-0335-tiered-memory
**Date:** 2026-02-22
**Status:** PlanRecord: CONFIRMED (Claude=SAT, Codex=SAT)

---

## Overview

Overhaul the zeroclaw memory system with a three-tier hierarchy: Short-Term (STM), Medium-Term (MTM), and Long-Term (LTM). Each tier serves a distinct role in the memory pipeline. The implementation is a `TieredMemory` wrapper around three independent `Memory` trait implementations — a drop-in replacement for existing backends.

---

## Tier Definitions

| Tier | Role | Prompt injection | Retention |
|------|------|-----------------|-----------|
| **STM** | Raw recent entries + semantic-tag indexes | Always — full content + index entries | 24h rolling window |
| **MTM** | LLM-compressed daily summaries + LTM indexes | Always — under token budget | Token-budget-based |
| **LTM** | Full-detail archive | On-demand via search | Permanent |

### Index Entry Format (STM)
Semantic tags + timestamp + reference IDs, grouped by topic:
```
[auth, middleware] 2026-01-15 → MTM:mtm-2026-01-15, LTM:auth-2026-01-15
[database, perf]  2026-01-14 → MTM:mtm-2026-01-14, LTM:db-2026-01-14
```

---

## Architecture

```
TieredMemory (implements Memory trait — drop-in)
├── stm: Arc<dyn Memory>     ← SQLite, 24h window
├── mtm: Arc<dyn Memory>     ← SQLite, token-budget window
└── ltm: Arc<dyn Memory>     ← existing zeroclaw backend (unchanged)

TierManager (background tokio task)
├── run_daily_compression()  ← STM→MTM at 23:59 local time
├── check_mtm_budget()       ← MTM→LTM when budget exceeded
└── build_index_entry()      ← semantic tag index entries in STM

TierConfig
├── stm_window: Duration             (default: 24h)
├── stm_eod_hour: u8                 (default: 23)
├── stm_eod_minute: u8               (default: 59)
├── user_location: Option<String>    (e.g. "Cebu, Philippines")
├── resolved_timezone: String        (IANA, e.g. "Asia/Manila")
├── mtm_token_budget: usize          (default: 2000)
├── mtm_budget_hysteresis: usize
├── mtm_max_batch_days: usize
├── compression_retry_max: u32
├── compression_retry_base_backoff_secs: u64
├── recall_top_k_per_tier: usize
├── recall_final_top_k: usize
├── weight_stm: f32                  (default: 0.45)
├── weight_mtm: f32                  (default: 0.35)
├── weight_ltm: f32                  (default: 0.20)
└── min_relevance_threshold: f32     (default: 0.4)
```

---

## Compression Pipeline

### STM → MTM (End-of-day)
1. TierManager wakes at 23:59 user-local time
2. Fetches all STM entries for current day
3. LLM summarizes entries → single MTM `MemoryEntry` (compressed daily summary)
4. Archives raw STM entries to LTM (full detail preserved)
5. Writes `IndexEntry` to STM: `[tags] date → MTM:id, LTM:ids`
6. Marks `CompressionJob` as succeeded

On failure: keep STM entries intact, mark job `Failed`, retry with exponential backoff.

### MTM → LTM (Budget overflow)
Triggered after each STM→MTM write and on a 15-minute sweep:
1. Compute total MTM token count
2. If `total > mtm_token_budget + hysteresis`: sort MTM oldest-first
3. Select batch until freed tokens ≥ overflow + hysteresis (capped by `mtm_max_batch_days`)
4. LLM compresses batch → single LTM entry
5. Delete selected MTM entries; update STM index refs to point directly to LTM
6. Retry with backoff on failure; degrade to one-at-a-time after max retries

All operations idempotent via `CompressionJob` journal with `job_id` + `source_entry_ids`.

---

## Concurrency Strategy (Single-Process)

- `compression_guard: Mutex<()>` — sole writer guard for the compression pipeline
- Never hold any lock across `.await` to LLM or DB I/O
- Snapshot first → release lock → call LLM → re-acquire for commit
- `recall()` calls STM/MTM/LTM concurrently with `tokio::join!`, merges in-memory
- `store()` is non-blocking relative to compression jobs

---

## Multi-Tier Recall Merging

```
final_score = tier_weight * semantic_score
            + 0.25 * recency_score
            + 0.15 * cross_tier_link_boost
```

- Tier weights: STM=0.45, MTM=0.35, LTM=0.20
- Link boost: STM index references an MTM/LTM candidate
- Dedup: by `origin_id` / `source_entry_ids` lineage
- Diversity guard: reserve ≥1 slot each for STM and MTM when available
- Filter: `final_score >= min_relevance_threshold` (0.4, matching existing loader)

---

## Timezone Resolution

- User provides `user_location` string (e.g. "Cebu, Philippines")
- At startup: system timezone set immediately as fallback
- Background task: LLM called with structured prompt, 2-3s timeout
  - Input hardened: trimmed, capped at 256 chars, control chars stripped, sent as JSON data field
  - Output: structured JSON → validated with `Region/City` regex + `chrono_tz::Tz` parse
  - On valid result: `resolved_timezone` updated; cached in config
  - On invalid/timeout: retain system timezone fallback
- Provenance tracked: `user_location` | `system_fallback` | `manual`
- Re-resolved only when `user_location` changes

---

## IndexEntry Schema

```rust
pub struct IndexEntry {
    pub id: String,                    // idx-<topic>-<date>
    pub topic: String,                 // primary group label
    pub tags: Vec<String>,             // 2-6 lowercase kebab-case tags
    pub day: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub mtm_ref_id: Option<String>,
    pub ltm_ref_id: Option<String>,
    pub source_entry_ids: Vec<String>, // raw STM IDs summarized
    pub confidence: f32,
    pub extractor_version: String,
}
```

Tag extraction: deterministic noun/keyphrase pass → embedding centroid labels → LLM tag-refiner if confidence < threshold.

---

## CompressionJob Schema

```rust
pub struct CompressionJob {
    pub id: String,
    pub kind: CompressionJobKind,      // StmDayToMtm | MtmOverflowToLtm
    pub status: CompressionJobStatus,  // Pending | Running | Succeeded | Failed
    pub attempts: u32,
    pub last_error: Option<String>,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Two-phase commit: `Running` marker persisted before work begins; only deleted/transitioned on confirmed write.

---

## File Layout

### New files
```
src/memory/tiered/mod.rs          — TieredMemory, Memory impl
src/memory/tiered/manager.rs      — TierManager, job loop, scheduler
src/memory/tiered/types.rs        — TierConfig, IndexEntry, CompressionJob, enums
src/memory/tiered/merge.rs        — recall merge/ranking algorithm
src/memory/tiered/tagging.rs      — TagExtractor trait + impls
src/memory/tiered/summarization.rs — TierSummarizer trait + LLM impl
src/memory/tiered/budget.rs       — token budget check + batch selection
```

### Modified files
```
src/memory/mod.rs                 — export tiered module, loader wiring
src/memory/backend.rs             — create_tiered_memory() factory
src/agent/memory_loader.rs        — TieredMemoryLoader (inject STM + MTM under budget)
Cargo.toml                        — no new deps required (chrono-tz already present)
```

### Unchanged
```
src/memory/sqlite.rs
src/memory/markdown.rs
src/memory/lucid.rs
src/memory/postgres.rs
src/memory/hygiene.rs             — hygiene scheduler delegates STM EOD trigger to TierManager
web/src/pages/Memory.tsx
```

---

## Error Handling

| Failure | Behavior |
|---------|----------|
| STM→MTM LLM failure | Keep STM entries; mark job Failed; retry with backoff |
| MTM→LTM LLM failure | Keep MTM entries; mark over-budget; retry; degrade to one-at-a-time |
| Timezone LLM failure | Retain system timezone fallback; no startup block |
| Partial commit | Two-phase commit markers; replay non-terminal jobs on restart |
| recall() tier failure | Return results from available tiers; log degraded |
| health_check() | Returns degraded status with failed/pending job counts |

---

## Acceptance Criteria

1. `TieredMemory` implements `Memory` trait; usable as drop-in via `create_tiered_memory()`
2. STM entries and STM IndexEntries always appear in every prompt injection
3. Daily compression fires at 23:59 local time; STM→MTM→LTM pipeline produces correct artifacts
4. MTM token budget enforced; no prompt exceeds `mtm_token_budget` from MTM content
5. `recall()` returns merged ranked results from all three tiers
6. `health_check()` reports degraded on failed compression jobs
7. All compression operations idempotent (safe to replay)
8. LLM resolves "Cebu, Philippines" → "Asia/Manila"; falls back gracefully within 2-3s
9. Agent startup never blocked by timezone resolution

---

## Out of Scope

- Distributed multi-process locking
- Remote geocoding API
- Changes to existing Memory backends
- Web UI changes
- Migration tooling for existing data
