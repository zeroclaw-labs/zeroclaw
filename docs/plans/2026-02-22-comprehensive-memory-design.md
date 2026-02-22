# Comprehensive Memory Capture Enhancement Design

**Task ID:** 20260222-0335-tiered-memory
**Date:** 2026-02-22
**Status:** PlanRecord: CONFIRMED (Claude=SAT, Codex=SAT)

---

## Overview

Enhance the three-tier memory system (STM/MTM/LTM) with structured fact extraction, dedicated memory agents per tier, and agent response capture. The system extracts all facts, numbers, preferences, and details from every conversation turn and makes them reliably recallable.

---

## Architecture

```
Main Agent (user's chosen model — conversations only)
    │
    ├─ STM Memory Agent (gemini-3-flash via OpenRouter)
    │   ├─ Real-time fact extraction per turn (non-blocking)
    │   ├─ Input: last 3-5 turns (~2200 token cap)
    │   ├─ Output: structured FactEntry JSON array
    │   └─ Stores facts as MemoryCategory::Core
    │
    ├─ MTM Memory Agent (gemini-3-flash via OpenRouter)
    │   ├─ Daily compression at 23:59 local time
    │   ├─ Input: full day transcript + targeted historical facts (~20k token cap)
    │   ├─ Deep extraction: cross-referencing, relationships, corrections
    │   └─ Produces daily summary + fact updates + contradiction flags
    │
    └─ LTM Memory Agent (gemini-3-flash via OpenRouter)
        ├─ MTM→LTM compression on budget overflow
        ├─ Merges daily summaries into archival entries
        └─ Preserves durable facts, compresses narrative
```

All memory agents use `google/gemini-3-flash-preview` via OpenRouter with separate API key config.

---

## FactEntry Schema

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FactConfidence { Low, Medium, High }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FactStatus { Active, Superseded, Retracted }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SourceRole { User, Agent, System }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VolatilityClass { Stable, SemiStable, Volatile }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceTurnRef {
    pub conversation_id: String,
    pub turn_index: u64,
    pub message_id: Option<String>,
    pub role: SourceRole,
    pub timestamp_unix_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEntry {
    pub fact_id: String,                    // ULID
    pub fact_key: String,                   // fact:{category}:{subject}:{attribute}
    pub category: String,
    pub subject: String,
    pub attribute: String,
    pub value: String,
    pub context_narrative: String,
    pub source_turn: SourceTurnRef,
    pub confidence: FactConfidence,
    pub related_facts: Vec<String>,
    pub extracted_by_tier: String,          // "stm" | "mtm" | "ltm"
    pub extracted_at_unix_ms: i64,
    pub source_role: SourceRole,
    pub status: FactStatus,
    pub revision: u32,
    pub supersedes_fact_id: Option<String>,
    pub tags: Vec<String>,
    pub volatility_class: VolatilityClass,
    pub ttl_days: Option<u16>,
    pub expires_at_unix_ms: Option<i64>,
    pub last_verified_unix_ms: Option<i64>,
}
```

---

## Fact Key Taxonomy

- **Canonical key**: `fact:{category}:{subject_slug}:{attribute_slug}`
- **Multi-valued**: `fact:{category}:{subject}:{attribute}:{vhash8}` (8-char hash of value)
- **Examples**:
  - `fact:personal:user:birthday` → "March 15"
  - `fact:infra:main_server:ip_address` → "192.168.1.10"
  - `fact:preference:user:editor` → "neovim"
  - `fact:project:zeroclaw:language` → "Rust"
  - `fact:relationship:user:employer` → "Acme Corp"

### Collision Rules

1. Same key + same normalized value → merge context, bump `last_verified`, keep revision
2. Same key + different value → new revision, mark prior `Superseded` if newer evidence stronger
3. Lower-confidence agent fact never supersedes higher-confidence user fact
4. Corrections create new row with `supersedes_fact_id`; never destructive overwrite

---

## Dual-Pass Fact Extraction

### Real-Time STM Pass (per conversation turn)

1. User sends message → auto-saved as `Conversation` entry
2. Agent responds → auto-saved as `Conversation` with key `msg:agent:{conv_id}:{turn_idx}`
3. Both messages enqueued to STM extraction queue (bounded `tokio::mpsc`)
4. STM Memory Agent receives prompt with:
   - Current turn (user + agent messages): ~900 tokens
   - Last 3-5 prior turns (truncated oldest-first): ~800 tokens
   - Instructions + JSON schema: ~350 tokens
   - **Total cap: ~2200 input tokens, ~400 output tokens**
5. Agent returns JSON array of `FactEntry` drafts
6. Facts stored as `MemoryCategory::Core` with searchable text in content field

**Non-blocking guarantee**: If queue is full, mark turn as `pending_mtm_only=true` and rely on MTM deep pass. Never block main agent response path.

### Deep MTM Pass (daily compression)

1. Loads ALL STM entries for the day (raw messages + extracted facts)
2. Loads targeted historical fact pack:
   - All facts from last 7 days
   - Older facts matched by tags/semantic similarity to today's conversation
   - Pinned stable profile facts (identity, high-confidence preferences)
   - **Capped at ~20k tokens**
3. MTM Memory Agent performs:
   - Cross-reference facts for consistency
   - Identify relationships between facts
   - Detect implicit preferences
   - Flag contradictions with existing facts
   - Correct STM extraction errors
4. Outputs: daily summary + fact updates + relationship links

### LTM Consolidation (budget overflow)

1. When MTM exceeds token budget, LTM agent compresses oldest MTM entries
2. Preserves all durable/high-signal facts
3. Compresses narrative, merges overlapping summaries
4. Marks volatile expired facts as stale

---

## Agent Response Auto-Save

### Changes
- Agent responses saved with key `msg:agent:{conversation_id}:{turn_index}`
- Legacy `is_assistant_autosave_key()` filter preserved — blocks `assistant_resp*` prefix (old data)
- New `msg:agent:*` prefix bypasses legacy filter

### Poisoning Protection
- Facts extracted from agent responses: `confidence` capped at `Medium` unless corroborated by user evidence
- `source_role: Agent` tracked on all agent-derived facts
- MTM deep pass cross-references agent facts against user-stated facts
- Recall can weight user-sourced facts higher than agent-sourced

---

## Enhanced Recall

### Fact-First Search
1. Query structured facts first (key match + semantic search on `context_narrative`)
2. Then search raw conversation entries
3. Then search MTM summaries + LTM archive

### Scoring Integration
Facts participate in existing `merge_and_rank` with additional boosts:
- `confidence`: High > Medium > Low
- `source_role`: User > System > Agent
- Recency: mild decay for stable facts, stronger for volatile
- Category boost from query intent (numeric query → boost numeric facts)

### Prompt Injection Format
```
## Known Facts
- [high] user's birthday: March 15 (stated while discussing deployment schedule)
- [high] main server IP: 192.168.1.10 (mentioned during infrastructure setup)
- [medium] preferred editor: neovim (observed from multiple sessions)
```
- Top-N facts by relevance, grouped by confidence
- Exclude `Low` confidence unless no better alternatives
- Flag contradictions explicitly

### Recall Limits
- Facts: up to 15 entries (increased from 5)
- Raw conversation: up to 5 entries (unchanged)
- MTM/LTM: per existing tier weights

---

## OpenRouter Client

### Configuration
```
[memory.agents]
openrouter_api_key = "sk-or-..."
openrouter_base_url = "https://openrouter.ai/api/v1"

[memory.agents.stm]
model = "google/gemini-3-flash-preview"
max_tokens = 400
temperature = 0.1
timeout_secs = 10

[memory.agents.mtm]
model = "google/gemini-3-flash-preview"
max_tokens = 8192
temperature = 0.1
timeout_secs = 60

[memory.agents.ltm]
model = "google/gemini-3-flash-preview"
max_tokens = 8192
temperature = 0.1
timeout_secs = 60
```

### Client Design
- Standalone `MemoryExtractionClient` — not coupled to Provider trait
- Trait: `FactExtractor { async fn extract(&self, prompt: &str) -> Result<Vec<FactEntryDraft>> }`
- Implementations: `OpenRouterFactExtractor` (production) + `MockFactExtractor` (tests)
- OpenAI-compatible HTTP client (OpenRouter uses same API format)

---

## Concurrency Model

- STM extraction queue: bounded `tokio::mpsc` channel
- Worker pool: 2-4 concurrent extraction tasks
- Backpressure: queue full → skip STM, mark `pending_mtm_only`
- Retry: exponential backoff + jitter on 429/5xx
- Hard timeout per call (STM: 10s, MTM/LTM: 60s)

---

## Fact Expiry

| Volatility Class | TTL | Examples |
|-----------------|-----|---------|
| `Stable` | None (permanent) | Birthday, name, native language |
| `SemiStable` | 90 days | Server configs, project tech stack |
| `Volatile` | 14 days | Current task, today's schedule |

- MTM agent assigns `volatility_class` during deep extraction
- Runtime maps class to deterministic TTL policy
- Expired facts: marked stale and down-ranked in recall (not deleted)

---

## Error Handling

| Failure | Behavior |
|---------|----------|
| OpenRouter timeout/5xx | Retry with backoff; skip STM extraction; MTM backstop |
| Rate limited (429) | Respect `Retry-After`; reduce concurrent workers |
| Invalid JSON response | One repair attempt ("return valid JSON only"); drop if still invalid |
| Queue overflow | Mark turn `pending_mtm_only`; MTM picks it up |
| All agents down | Memory system degrades to raw message storage only (existing behavior) |

Never fail the main agent's response path because of memory extraction failure.

---

## File Layout

### New Files
```
src/memory/tiered/facts.rs         — FactEntry struct, key taxonomy, collision/supersession logic
src/memory/tiered/extractor.rs     — FactExtractor trait, OpenRouterFactExtractor, MockFactExtractor
src/memory/tiered/prompts.rs       — Extraction prompt templates for STM/MTM/LTM agents
```

### Modified Files
```
src/memory/tiered/types.rs         — Add MemoryAgentConfig to TierConfig
src/memory/tiered/manager.rs       — Wire real agents, fact extraction in compress_day()
src/memory/tiered/mod.rs           — Auto-save agent responses, trigger STM extraction queue
src/memory/tiered/merge.rs         — Add fact-type boosting to recall scoring
src/memory/tiered/loader.rs        — Include [Known Facts] section in prompt injection
src/agent/loop_.rs                 — Auto-save agent responses with msg:agent:* key prefix
src/config/schema.rs               — Add memory.agents config section
```

---

## Acceptance Criteria

1. FactEntry struct with all required fields implemented
2. OpenRouter MemoryExtractionClient sends requests to gemini-3-flash and parses JSON
3. STM extraction runs per turn without blocking main agent (bounded queue, graceful degradation)
4. STM prompt capped at ~2200 input tokens
5. Agent responses auto-saved with `msg:agent:*` key prefix (legacy filter preserved)
6. MTM compression includes deep fact extraction with targeted historical facts (~20k cap)
7. LTM compression preserves durable facts
8. Facts stored as Core entries in existing Memory backend
9. Fact key taxonomy: `fact:{category}:{subject}:{attribute}` with vhash8 for multi-valued
10. Revision chain: corrections create new revision with supersedes_fact_id
11. Agent-derived facts capped at confidence=Medium unless corroborated
12. Fact-first recall integrated into merge_and_rank with confidence/source/recency boosts
13. Facts injected into prompt in Known Facts section
14. Optional TTL with volatility_class mapped to deterministic policy
15. Error handling: retry, repair, never block main agent
16. All existing tests pass (0 regressions)

---

## Out of Scope

- Web UI changes
- Changes to existing Memory backends
- Distributed multi-process extraction
- Custom embedding model training
- Migration tooling for existing memory data
