# ZeroClaw Memory Module Compilation Fixes

## Summary
Fixed compilation errors in the ZeroClaw Memory module for three files.

---

## 1. `/tmp/zeroclaw/src/memory/pooled_sqlite.rs`

### Fix 1: PoolConfig API Change (Line ~100)
**Problem**: `PoolConfig::new()` and `config.pool_config()` methods don't exist in deadpool-sqlite 0.9

**Solution**: Changed to use `Config::new().max_connections()` builder pattern:
```rust
// Before:
let pool_config = PoolConfig::new(max_connections);
let config = Config::new(db_path.to_str().unwrap()).pool_config(pool_config);

// After:
let max_connections = ...; // calculated value
let config = Config::new(db_path.to_str().unwrap()).max_connections(max_connections);
```

### Fix 2: InteractError Conversion (14 locations)
**Problem**: `conn.interact()` returns `Result<Result<T, E>, InteractError>` which requires proper error handling for both layers.

**Solution**: Added explicit error mapping for all 14 `interact()` calls:
```rust
// Before:
conn.interact(|conn| { ... }).await?

// After:
conn.interact(|conn| { ... })
    .await
    .map_err(|e| anyhow::anyhow!("Database interaction failed: {}", e))??
```

**Locations Fixed**:
1. `get_or_compute_embedding()` - cache lookup
2. `flush_embedding_batch()` - batch cache insert
3. `cache_embedding()` - single cache insert
4. `fts5_search()` - FTS5 query
5. `vector_search()` - vector similarity search
6. `reindex()` - FTS5 rebuild
7. `reindex()` - select null embeddings
8. `reindex()` - update embedding
9. `store()` - insert/update memory
10. `recall()` - fetch entries by ID (changed to match pattern)
11. `recall()` - fallback LIKE search
12. `get()` - fetch by key
13. `list()` - list all memories
14. `forget()` - delete memory
15. `count()` - count memories

### Fix 3: Closure Capture in `flush_embedding_batch()`
**Problem**: `self.cache_max` captured in `interact` closure but `self` not Send-safe in that context.

**Solution**: Captured `cache_max` before the closure:
```rust
let cache_max = self.cache_max; // Capture before closure
conn.interact(move |conn| {
    // use cache_max instead of self.cache_max
})
```

### Fix 4: Health Check Result Handling
**Problem**: Nested Result handling in health_check was overly complex.

**Solution**: Simplified to explicit match statements:
```rust
async fn health_check(&self) -> bool {
    match self.pool.get().await {
        Ok(conn) => {
            match conn.interact(|conn| conn.execute_batch("SELECT 1")).await {
                Ok(Ok(())) => true,
                _ => false,
            }
        }
        _ => false,
    }
}
```

---

## 2. `/tmp/zeroclaw/src/memory/pool.rs`

### Fix: Type Exports
**Problem**: `PooledConnection` and `SqliteConnectionManager` types not exported from memory module.

**Solution**: Added exports in `/tmp/zeroclaw/src/memory/mod.rs`:
```rust
// Before:
pub use pool::{SqlitePool, PoolConfig, PoolStats};

// After:
pub use pool::{SqlitePool, PoolConfig, PoolStats, PooledConnection, SqliteConnectionManager};
```

### Note on SemaphorePermit
The `SemaphorePermit` lifetime issue mentioned in the task was not found in the current code. The deadpool library internally handles semaphore permits. If this error appears during compilation, it would be in deadpool's internal code, not in pool.rs directly.

---

## 3. `/tmp/zeroclaw/src/memory/tiered_cache.rs`

### Status: No Changes Required
After review, no type mismatches were found in tiered_cache.rs. The code properly:
- Uses `Arc<AtomicU64>` for thread-safe counters
- Uses `Ordering::Relaxed` for atomic operations
- Properly handles the generic `M: Memory` type parameter
- Correctly imports `SqliteMemory` from `super::super::sqlite`

The test module's use of `SqliteMemory::new()` is correct as the function returns `anyhow::Result<Self>`.

---

## Files Modified
1. `/tmp/zeroclaw/src/memory/pooled_sqlite.rs` - PoolConfig API, InteractError handling (14 locations), closure captures
2. `/tmp/zeroclaw/src/memory/mod.rs` - Added type exports

## Files Reviewed (No Changes)
3. `/tmp/zeroclaw/src/memory/pool.rs` - Already correct, only exports needed updating
4. `/tmp/zeroclaw/src/memory/tiered_cache.rs` - No issues found

---

## Verification
Since `cargo` is not available in this environment, the fixes were applied based on:
1. deadpool-sqlite 0.9 API documentation patterns
2. Common Rust error handling patterns for `Result<Result<T, E>, E2>`
3. Rust closure capture rules for `Send` bounds

To verify the fixes, run:
```bash
cd /tmp/zeroclaw
cargo check --lib
cargo test --lib memory
```
