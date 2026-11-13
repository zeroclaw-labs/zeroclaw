use wasmtime::ResourceLimiter;

use crate::component::PluginLimits;

/// Allocation reservations for the lifetime of one store, across all its
/// memories and tables. Refueling an export does not replenish these budgets.
///
/// Keep reservations after allocation failures. Wasmtime can call the failure
/// callbacks without a preceding growth callback, so refunding the last
/// reservation could refund an already successful allocation.
pub(crate) struct PluginResourceLimiter {
    memory_bytes_remaining: usize,
    table_elements_remaining: usize,
    max_instances: usize,
}

impl PluginResourceLimiter {
    pub(crate) fn new(limits: PluginLimits) -> Self {
        Self {
            memory_bytes_remaining: limits.max_memory_bytes,
            table_elements_remaining: limits.max_table_elements,
            max_instances: limits.max_instances,
        }
    }
}

fn reserve(remaining: &mut usize, current: usize, desired: usize, maximum: Option<usize>) -> bool {
    if maximum.is_some_and(|maximum| desired > maximum) {
        return false;
    }
    let Some(delta) = desired.checked_sub(current) else {
        return false;
    };
    let Some(balance) = remaining.checked_sub(delta) else {
        return false;
    };
    *remaining = balance;
    true
}

impl ResourceLimiter for PluginResourceLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(reserve(
            &mut self.memory_bytes_remaining,
            current,
            desired,
            maximum,
        ))
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(reserve(
            &mut self.table_elements_remaining,
            current,
            desired,
            maximum,
        ))
    }

    fn instances(&self) -> usize {
        self.max_instances
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::test_limits;

    #[test]
    fn rejected_growth_does_not_consume_remaining_budget() {
        let mut limiter = PluginResourceLimiter::new(test_limits(1_000));
        let bytes = test_limits(1_000).max_memory_bytes;
        let elements = test_limits(1_000).max_table_elements;
        assert!(!limiter.memory_growing(0, bytes, Some(1)).unwrap());
        assert!(!limiter.memory_growing(1, 0, None).unwrap());
        assert!(!limiter.memory_growing(0, usize::MAX, None).unwrap());
        assert!(limiter.memory_growing(0, bytes, None).unwrap());
        assert!(!limiter.memory_growing(0, 1, None).unwrap());
        assert!(!limiter.table_growing(0, elements, Some(1)).unwrap());
        assert!(!limiter.table_growing(1, 0, None).unwrap());
        assert!(!limiter.table_growing(0, usize::MAX, None).unwrap());
        assert!(limiter.table_growing(0, elements, None).unwrap());
        assert!(!limiter.table_growing(0, 1, None).unwrap());
    }

    #[test]
    fn failure_callbacks_cannot_refund_successful_allocations() {
        let mut limiter = PluginResourceLimiter::new(test_limits(1_000));
        assert!(
            limiter
                .memory_growing(0, test_limits(1_000).max_memory_bytes, None)
                .unwrap()
        );
        assert!(
            limiter
                .table_growing(0, test_limits(1_000).max_table_elements, None)
                .unwrap()
        );
        limiter
            .memory_grow_failed(wasmtime::Error::msg("invalid memory growth"))
            .unwrap();
        limiter
            .table_grow_failed(wasmtime::Error::msg("table growth overflow"))
            .unwrap();
        assert!(!limiter.memory_growing(0, 1, None).unwrap());
        assert!(!limiter.table_growing(0, 1, None).unwrap());
    }
}
