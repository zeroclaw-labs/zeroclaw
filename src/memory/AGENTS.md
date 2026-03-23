# AGENTS.md — memory/

> Persistence, retrieval, and knowledge management backends.

## Overview

The memory subsystem provides durable storage, semantic retrieval, and lifecycle management for agent knowledge. All backends implement the `Memory` trait (`traits.rs`). The factory in `mod.rs` selects backends via `MemoryConfig.backend` and optional `StorageProviderConfig` override. Unknown backends silently fall back to markdown.

## Key Files

| File | Role |
|---|---|
| `traits.rs` | `Memory` trait, `MemoryEntry`, `MemoryCategory`, `ProceduralMessage` |
| `mod.rs` | Factory (`create_memory*`), embedding config resolution, autosave filtering |
| `backend.rs` | `MemoryBackendKind` enum, `MemoryBackendProfile` metadata, `classify_memory_backend()` |
| `sqlite.rs` | Primary backend: WAL-mode SQLite, FTS5, vector BLOB storage, hybrid search, embedding cache |
| `markdown.rs` | File-based backend: `MEMORY.md` (core) + `memory/YYYY-MM-DD.md` (daily append-only) |
| `lucid.rs` | Bridge to external `lucid` CLI; delegates to local SQLite on timeout/failure with cooldown |
| `qdrant.rs` | Qdrant REST API backend with lazy collection init via `OnceCell` |
| `postgres.rs` | Feature-gated (`memory-postgres`); requires `db_url` in storage provider config |
| `none.rs` | Explicit no-op backend for disabling persistence while keeping wiring stable |
| `embeddings.rs` | `EmbeddingProvider` trait + `NoopEmbedding` + `OpenAiEmbedding` (OpenAI-compatible) |
| `vector.rs` | Cosine similarity, LE byte serialization, weighted hybrid merge (vector + BM25) |
| `retrieval.rs` | 3-stage pipeline: hot LRU cache -> FTS5 (with early-return threshold) -> vector search |
| `consolidation.rs` | LLM-driven two-phase extraction: daily history entry + optional Core memory update |
| `decay.rs` | Exponential time decay (`2^(-age/half_life)`); Core memories are exempt ("evergreen") |
| `importance.rs` | Heuristic scorer (category base + keyword boost); final score = 0.7*hybrid + 0.2*importance + 0.1*recency |
| `conflict.rs` | Semantic dedup for Core entries: cosine similarity > threshold marks old entry `superseded_by` |
| `policy.rs` | Pre-backend validation: read-only namespaces, per-namespace/category quotas, retention rules |
| `hygiene.rs` | Throttled (12h) cleanup: archive old daily files, prune conversation rows, respect policy retention |
| `snapshot.rs` | Export Core memories to `MEMORY_SNAPSHOT.md`; auto-hydrate on cold boot if `brain.db` missing |
| `chunker.rs` | Markdown-aware text chunking: split on headings -> paragraphs -> lines; ~4 chars/token estimate |
| `audit.rs` | `AuditedMemory<M>` decorator logging all ops to `audit.db`; opt-in via config |
| `knowledge_graph.rs` | SQLite graph of nodes (pattern/decision/lesson/expert/technology) + directed edges + FTS |
| `response_cache.rs` | Two-tier (in-memory LRU + SQLite) response cache keyed by SHA-256 of prompt; TTL-based expiry |

## Trait Contract

`Memory` (in `traits.rs`) requires `Send + Sync`. Required methods: `name`, `store`, `recall`, `get`, `list`, `forget`, `count`, `health_check`. Optional methods with default impls: `store_procedural` (no-op), `recall_namespaced` (filters post-recall; override for native namespace support), `store_with_metadata` (delegates to `store`; override for native namespace/importance). The `recall` method accepts `since`/`until` as RFC 3339 strings for time-range filtering.

## Extension Playbook

1. Create `src/memory/mybackend.rs` implementing `Memory` trait (all 8 required methods).
2. Add `pub mod mybackend;` to `mod.rs` and a `pub use` if needed.
3. Add a variant to `MemoryBackendKind` in `backend.rs` and a corresponding `MemoryBackendProfile` const.
4. Update `classify_memory_backend()` match arm and `selectable_memory_backends()`.
5. Wire into `create_memory_with_builders()` in `mod.rs`. Feature-gate if it pulls heavy deps (`#[cfg(feature = "memory-X")]`).
6. Add factory test (see existing `factory_sqlite`, `factory_markdown` patterns in `mod.rs` tests).
7. If the backend needs embedding, accept `Arc<dyn EmbeddingProvider>` and reuse `resolve_embedding_config()`.

## Factory Registration

`create_memory_with_storage_and_routes()` is the canonical factory entry point. It resolves the effective backend name (config vs storage provider override), runs hygiene/snapshot passes, then dispatches via `create_memory_with_builders()`. Embedding config resolution handles `hint:` prefix routing through `EmbeddingRouteConfig` entries. API key precedence: provider-specific env var (e.g. `COHERE_API_KEY`) > caller-supplied key (which may belong to a different provider).

## Storage Backends

- **SQLite** (`brain.db`): WAL mode, FTS5 virtual table for BM25, embedding BLOBs for vector search, LRU embedding cache (default 10k). Configurable `vector_weight`/`keyword_weight` (default 0.7/0.3). Open timeout capped at 300s. Uses `parking_lot::Mutex<Connection>` (single-writer).
- **Markdown**: Zero-dependency. Core in `MEMORY.md`, daily logs in `memory/YYYY-MM-DD.md`. Keyword search is substring-based (no vector support). Good for human inspection.
- **Lucid**: Wraps SQLite + external `lucid` CLI. Falls back to local SQLite on CLI timeout (500ms recall, 800ms store) or failure (15s cooldown). Env-configurable thresholds.
- **Qdrant**: REST API, lazy collection init. Requires `QDRANT_URL` or config. Full vector search.
- **Postgres**: Feature-gated. Needs `db_url`, `schema`, `table` from `StorageProviderConfig`. Migration not supported.
- **None**: All methods return empty/Ok. `auto_save_default = false`.

## Retrieval Pipeline

`RetrievalPipeline` wraps any `Memory` with staged retrieval: (1) hot LRU cache (256 entries, 5min TTL), (2) FTS5 keyword search with early-return if score > 0.85, (3) vector similarity + hybrid merge. The `hybrid_merge()` function normalizes each score set to [0,1], fuses with configurable weights, deduplicates by id. Time decay is applied post-retrieval: `score * 2^(-age_days / half_life)` where `half_life` defaults to 7 days. Core memories skip decay entirely.

## Consolidation & Decay

Consolidation runs fire-and-forget via `tokio::spawn` after each turn. LLM extracts `history_entry` (Daily category) and optional `memory_update` (Core category) from a truncated turn (4000 char cap, UTF-8 boundary safe). Before storing Core updates, `conflict::check_and_resolve_conflicts` marks semantically similar existing entries as superseded. Importance scoring: heuristic path uses category base (Core=0.7, Daily=0.3, Conversation=0.2) plus keyword boost (capped at +0.2).

## Testing Patterns

- Factory tests: `TempDir` + default `MemoryConfig` with backend name -> assert `mem.name()`. See `mod.rs::tests`.
- Decay tests: construct `MemoryEntry` with known timestamps, call `apply_time_decay`, assert Core untouched.
- Conflict tests: mock recall results, verify `superseded_by` marking.
- Use `NoopEmbedding` for tests not exercising vector search. Use `SqliteMemory::new()` (no embedder) for pure keyword tests.
- `battle_tests` module exists for cross-backend integration tests.
- Autosave filter tests: verify `should_skip_autosave_content` rejects cron/heartbeat/distilled noise.

## Common Gotchas

- **Embedding key leakage**: Default provider API key may differ from embedding provider. The factory resolves this via `embedding_provider_env_key()` — always check the provider-specific env var first (issue #3083).
- **`hint:` prefix**: `embedding_model = "hint:semantic"` triggers route lookup, not direct model usage. Missing route falls back to base config.
- **Namespace filtering**: Default `recall_namespaced` over-fetches by 2x then filters client-side. Override for backends with native namespace support to avoid O(n) waste.
- **Single-writer SQLite**: `parking_lot::Mutex<Connection>` means concurrent writes block. WAL mode helps reads but writes are serialized.
- **Hygiene is best-effort**: `run_if_due` failures are logged and swallowed. Never let hygiene failure block backend creation.
- **Snapshot hydration**: Only triggers when `brain.db` is missing AND `MEMORY_SNAPSHOT.md` exists AND `auto_hydrate = true`. Don't delete `MEMORY_SNAPSHOT.md` from Git.
- **Migration restrictions**: `create_memory_for_migration` rejects `none` and `postgres` backends.

## Cross-Subsystem Coupling

- **`providers/`**: Consolidation calls `Provider::chat_with_system` for LLM extraction. Embedding providers are instantiated via `embeddings::create_embedding_provider`.
- **`config/`**: `MemoryConfig`, `MemoryPolicyConfig`, `EmbeddingRouteConfig`, `StorageProviderConfig` drive all behavior. `build_runtime_proxy_client` handles HTTP proxy for embedding calls.
- **`agent/`**: Orchestration loop calls `consolidate_turn` after each conversation turn and injects recalled memories into context.
- **`tools/`**: Memory tool surface (`memory_store`, `memory_recall`) delegates to the `Memory` trait object held by the agent.
