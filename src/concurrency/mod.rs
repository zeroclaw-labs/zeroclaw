//! ZeroClaw Concurrency Architecture
//!
//! 提供高性能异步并发原语：
//! - Worker Pool: 管理异步任务执行
//! - Backpressure: 基于 Semaphore 的背压机制
//! - Deduplicator: 请求去重
//! - Circuit Breaker: 熔断器保护

pub mod worker_pool;
pub mod backpressure;
pub mod deduplicator;
pub mod circuit_breaker;
pub mod channel_integration;

pub use worker_pool::{WorkerPool, Task, TaskResult, TaskPriority, WorkerPoolStats};
pub use backpressure::{Backpressure, BackpressureStats, RateLimiter, AdaptiveLimiter};
pub use deduplicator::{Deduplicator, DedupKey, DedupStrategy, DedupStats};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerGroup, CircuitBreakerStats, CircuitState, CircuitConfig};

use std::time::Duration;

/// 默认 Worker Pool 配置
pub const DEFAULT_WORKER_POOL_SIZE: usize = 4;
pub const DEFAULT_TASK_QUEUE_SIZE: usize = 100;
pub const DEFAULT_TASK_TIMEOUT: Duration = Duration::from_secs(30);

/// 默认背压配置
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 10;
pub const DEFAULT_RATE_LIMIT_PER_SEC: u32 = 100;

/// 默认熔断器配置
pub const DEFAULT_FAILURE_THRESHOLD: u32 = 5;
pub const DEFAULT_SUCCESS_THRESHOLD: u32 = 3;
pub const DEFAULT_CIRCUIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 并发管理器 - 整合所有并发控制组件
pub struct ConcurrencyManager {
    /// Worker Pool 用于执行任务
    pub worker_pool: WorkerPool,
    /// 背压控制器
    pub backpressure: Backpressure,
    /// 请求去重器
    pub deduplicator: Deduplicator,
    /// 熔断器
    pub circuit_breaker: CircuitBreaker,
}

impl ConcurrencyManager {
    /// 创建新的并发管理器
    pub fn new() -> Self {
        Self {
            worker_pool: WorkerPool::new(DEFAULT_WORKER_POOL_SIZE, DEFAULT_TASK_QUEUE_SIZE),
            backpressure: Backpressure::new(DEFAULT_MAX_CONCURRENT_REQUESTS),
            deduplicator: Deduplicator::new(Duration::from_secs(60)),
            circuit_breaker: CircuitBreaker::new(CircuitConfig::default()),
        }
    }

    /// 使用自定义配置创建
    pub fn with_config(
        worker_pool_size: usize,
        task_queue_size: usize,
        max_concurrent: usize,
        dedup_ttl: Duration,
        circuit_config: CircuitConfig,
    ) -> Self {
        Self {
            worker_pool: WorkerPool::new(worker_pool_size, task_queue_size),
            backpressure: Backpressure::new(max_concurrent),
            deduplicator: Deduplicator::new(dedup_ttl),
            circuit_breaker: CircuitBreaker::new(circuit_config),
        }
    }

    /// 关闭并发管理器
    pub async fn shutdown(self) {
        self.worker_pool.shutdown().await;
    }
}

impl Default for ConcurrencyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_WORKER_POOL_SIZE, 4);
        assert_eq!(DEFAULT_TASK_QUEUE_SIZE, 100);
        assert_eq!(DEFAULT_MAX_CONCURRENT_REQUESTS, 10);
    }

    #[test]
    fn test_concurrency_manager_new() {
        let manager = ConcurrencyManager::new();
        assert_eq!(manager.worker_pool.worker_count(), DEFAULT_WORKER_POOL_SIZE);
    }

    #[test]
    fn test_concurrency_manager_with_config() {
        let manager = ConcurrencyManager::with_config(
            8,                              // worker_pool_size
            200,                            // task_queue_size
            20,                             // max_concurrent
            Duration::from_secs(120),       // dedup_ttl
            CircuitConfig::default(),       // circuit_config
        );
        assert_eq!(manager.worker_pool.worker_count(), 8);
    }
}
