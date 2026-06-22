//! `wasmtime::ResourceLimiter` enforcing a per-execution memory ceiling.
//!
//! Ported from `ironclaw_wasm_limiter`. Tracks aggregate linear-memory growth
//! across all of a component's memories against a single byte budget, and caps
//! table/instance/memory counts. (The `tracing` diagnostics from the original
//! are dropped to avoid pulling a new dependency; the enforcement is identical.)

use wasmtime::ResourceLimiter;

#[derive(Debug)]
pub struct WasmResourceLimiter {
    memory_limit: u64,
    memory_used: u64,
    pending_memory_growth: u64,
    max_tables: u32,
    max_instances: u32,
    max_memories: u32,
}

impl WasmResourceLimiter {
    pub fn new(memory_limit: u64) -> Self {
        Self {
            memory_limit,
            memory_used: 0,
            pending_memory_growth: 0,
            max_tables: 10,
            max_instances: 10,
            max_memories: 10,
        }
    }
}

impl ResourceLimiter for WasmResourceLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        self.pending_memory_growth = 0;

        let current = current as u64;
        let desired = desired as u64;
        let growth = desired.saturating_sub(current);
        let total_memory = self.memory_used.saturating_add(growth);
        if total_memory > self.memory_limit {
            return Ok(false);
        }

        self.memory_used = total_memory;
        self.pending_memory_growth = growth;
        Ok(true)
    }

    fn memory_grow_failed(&mut self, _error: wasmtime::Error) -> Result<(), wasmtime::Error> {
        self.memory_used = self.memory_used.saturating_sub(self.pending_memory_growth);
        self.pending_memory_growth = 0;
        Ok(())
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        if desired > 10_000 {
            return Ok(false);
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        self.max_instances as usize
    }

    fn tables(&self) -> usize {
        self.max_tables as usize
    }

    fn memories(&self) -> usize {
        self.max_memories as usize
    }
}

#[cfg(test)]
mod tests {
    use wasmtime::ResourceLimiter;

    use super::WasmResourceLimiter;

    #[test]
    fn memories_limit_allows_component_model_internal_memories() {
        let limiter = WasmResourceLimiter::new(1024);
        assert_eq!(limiter.instances(), 10);
        assert_eq!(limiter.tables(), 10);
        assert_eq!(limiter.memories(), 10);
    }

    #[test]
    fn memory_growing_tracks_aggregate_growth_across_memories() {
        let mut limiter = WasmResourceLimiter::new(128 * 1024);
        assert!(limiter.memory_growing(0, 64 * 1024, None).unwrap());
        assert!(limiter.memory_growing(0, 64 * 1024, None).unwrap());
        assert!(!limiter.memory_growing(0, 64 * 1024, None).unwrap());
    }

    #[test]
    fn memory_grow_failed_rolls_back_pending_growth() {
        // When wasmtime approves a `memory_growing` request the limiter stages
        // the growth and bumps `memory_used`. If the OS-level grow then fails,
        // `memory_grow_failed` must unwind that bookkeeping so a later grow up
        // to the full ceiling can still succeed.
        let mut limiter = WasmResourceLimiter::new(64 * 1024);
        assert!(limiter.memory_growing(0, 32 * 1024, None).unwrap());
        let _ = limiter.memory_grow_failed(wasmtime::Error::msg("simulated ENOMEM"));
        assert!(
            limiter.memory_growing(0, 64 * 1024, None).unwrap(),
            "memory_grow_failed must release the pending growth so a retry up to the full ceiling can succeed"
        );
    }
}
