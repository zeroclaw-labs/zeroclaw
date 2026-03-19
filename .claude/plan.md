# Plan: Add mem0 Memory Backend (Feature-gated)

## Overview

Add mem0 as a new memory backend behind `--features memory-mem0` feature flag, following the same pattern as `memory-postgres` and `whatsapp-web`.

mem0 (OpenMemory) exposes a REST API at `http://localhost:8765/api/v1/memories/`. We integrate as an HTTP client — no Python dependency needed.

## Architecture

```
ZeroClaw Memory Trait
    ├── sqlite (default)
    ├── markdown
    ├── lucid
    ├── postgres (feature: memory-postgres)
    ├── qdrant
    ├── none
    └── mem0 (feature: memory-mem0)  ← NEW
```

mem0 backend calls the OpenMemory REST API via `reqwest`. It implements the `Memory` trait by mapping:

| Memory trait method | mem0 REST endpoint |
|--------------------|--------------------|
| `store()` | `POST /api/v1/memories/` |
| `recall()` | `GET /api/v1/memories/?search_query=...` |
| `get()` | `GET /api/v1/memories/{id}` |
| `list()` | `GET /api/v1/memories/?user_id=...` |
| `forget()` | `DELETE /api/v1/memories/` |
| `count()` | `GET /api/v1/memories/?user_id=...` (total from pagination) |
| `health_check()` | `GET /api/v1/memories/?page=1&size=1` (check 200 OK) |

## Files to Create/Modify

### New file
- `src/memory/mem0.rs` — `Mem0Memory` struct implementing `Memory` trait

### Modify
1. **`Cargo.toml`** — add `memory-mem0 = []` feature (no extra deps, `reqwest` already in deps)
2. **`src/memory/mod.rs`** — add `#[cfg(feature = "memory-mem0")] pub mod mem0;` + factory wiring
3. **`src/memory/backend.rs`** — add `Mem0` variant to `MemoryBackendKind`, profile, and classification
4. **`src/config/schema.rs`** — add `Mem0Config` struct (url, user_id, app_name) nested in `MemoryConfig`
5. **`docs/reference/api/config-reference.md`** + zh-CN + vi — document new config fields

## Config Schema

```toml
[memory]
backend = "mem0"

[memory.mem0]
url = "http://localhost:8765"    # OpenMemory server URL
user_id = "zeroclaw"             # mem0 user scoping
app_name = "zeroclaw"            # mem0 app identifier
```

## Implementation Steps

1. Add `Mem0Config` to `src/config/schema.rs`
2. Add `MemoryBackendKind::Mem0` to `src/memory/backend.rs`
3. Create `src/memory/mem0.rs` with `Mem0Memory` struct
4. Wire factory in `src/memory/mod.rs` (dual-impl pattern like postgres)
5. Add feature flag to `Cargo.toml`
6. Add tests
7. Update docs (3 locales)

## Feature Flag Pattern (following postgres)

```rust
// src/memory/mod.rs
#[cfg(feature = "memory-mem0")]
pub mod mem0;

// Factory — dual impl
#[cfg(feature = "memory-mem0")]
fn build_mem0_memory(config: &MemoryConfig) -> anyhow::Result<Box<dyn Memory>> {
    Ok(Box::new(mem0::Mem0Memory::new(&config.mem0)?))
}

#[cfg(not(feature = "memory-mem0"))]
fn build_mem0_memory(_config: &MemoryConfig) -> anyhow::Result<Box<dyn Memory>> {
    anyhow::bail!("mem0 memory backend requires `--features memory-mem0`")
}
```

## Build Commands

```bash
cargo build --features memory-mem0
cargo test --features memory-mem0
cargo clippy --features memory-mem0 --all-targets -- -D warnings
```

## Risk: Low-Medium

- New module, no existing code modified beyond factory wiring
- Feature-gated, won't affect default builds
- Uses existing `reqwest` dependency (no new deps)
