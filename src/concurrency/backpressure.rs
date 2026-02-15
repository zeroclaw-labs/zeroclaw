//! Backpressure - 背压机制
//!
//! 使用 Semaphore 实现请求限流和并发控制：
//! - 限制同时处理的请求数
//! - 自适应限流
//! - 令牌桶算法
//! - 队列管理

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::interval;
use tracing::{debug, trace, warn};

/// 背压控制器
pub struct Backpressure {
    /// 并发信号量 - 限制同时处理的请求数
    semaphore: Arc<Semaphore>,
    /// 最大并发数
    max_concurrent: usize,
    /// 当前等待的请求数
    waiting_count: AtomicUsize,
    /// 被拒绝的请求数
    rejected_count: AtomicU64,
    /// 总请求数
    total_count: AtomicU64,
}

impl Backpressure {
    /// 创建新的背压控制器
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            waiting_count: AtomicUsize::new(0),
            rejected_count: AtomicU64::new(0),
            total_count: AtomicU64::new(0),
        }
    }

    /// 获取一个执行许可（阻塞直到获得）
    pub async fn acquire(&self) -> BackpressurePermit {
        self.total_count.fetch_add(1, Ordering::SeqCst);
        self.waiting_count.fetch_add(1, Ordering::SeqCst);
        
        let permit = self.semaphore.clone().acquire_owned().await;
        
        self.waiting_count.fetch_sub(1, Ordering::SeqCst);
        
        BackpressurePermit {
            _permit: permit.ok(),
            acquired_at: Instant::now(),
        }
    }

    /// 尝试获取一个执行许可（非阻塞）
    pub fn try_acquire(&self) -> Option<BackpressurePermit> {
        self.total_count.fetch_add(1, Ordering::SeqCst);
        
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(BackpressurePermit {
                _permit: Some(permit),
                acquired_at: Instant::now(),
            }),
            Err(_) => {
                self.rejected_count.fetch_add(1, Ordering::SeqCst);
                None
            }
        }
    }

    /// 获取多个许可
    pub async fn acquire_many(&self, n: u32) -> Option<BackpressurePermit> {
        if n as usize > self.max_concurrent {
            return None;
        }
        
        self.total_count.fetch_add(1, Ordering::SeqCst);
        self.waiting_count.fetch_add(1, Ordering::SeqCst);
        
        let permit = self.semaphore.clone().acquire_many_owned(n).await;
        
        self.waiting_count.fetch_sub(1, Ordering::SeqCst);
        
        Some(BackpressurePermit {
            _permit: permit.ok(),
            acquired_at: Instant::now(),
        })
    }

    /// 检查当前是否可以立即获得许可
    pub fn can_acquire(&self) -> bool {
        self.semaphore.available_permits() > 0
    }

    /// 获取当前可用许可数
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// 获取当前等待的请求数
    pub fn waiting_count(&self) -> usize {
        self.waiting_count.load(Ordering::SeqCst)
    }

    /// 获取被拒绝的请求数
    pub fn rejected_count(&self) -> u64 {
        self.rejected_count.load(Ordering::SeqCst)
    }

    /// 获取总请求数
    pub fn total_count(&self) -> u64 {
        self.total_count.load(Ordering::SeqCst)
    }

    /// 获取当前活跃请求数
    pub fn active_count(&self) -> usize {
        self.max_concurrent.saturating_sub(self.semaphore.available_permits())
    }

    /// 获取当前负载百分比 (0-100)
    pub fn load_percentage(&self) -> u8 {
        let active = self.active_count();
        ((active as f64 / self.max_concurrent as f64) * 100.0) as u8
    }
}

impl Clone for Backpressure {
    fn clone(&self) -> Self {
        Self {
            semaphore: Arc::clone(&self.semaphore),
            max_concurrent: self.max_concurrent,
            waiting_count: AtomicUsize::new(0),
            rejected_count: AtomicU64::new(self.rejected_count.load(Ordering::SeqCst)),
            total_count: AtomicU64::new(self.total_count.load(Ordering::SeqCst)),
        }
    }
}

/// 背压许可 - 持有期间表示可以执行
pub struct BackpressurePermit {
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
    acquired_at: Instant,
}

impl BackpressurePermit {
    /// 获取持有许可的时间
    pub fn held_duration(&self) -> Duration {
        self.acquired_at.elapsed()
    }
}

/// 速率限制器 - 令牌桶算法
pub struct RateLimiter {
    /// 每秒生成的令牌数
    rate_per_sec: u32,
    /// 桶容量
    capacity: u32,
    /// 当前令牌数
    tokens: AtomicU64,
    /// 上次更新时间
    last_update: Mutex<Instant>,
}

impl RateLimiter {
    /// 创建新的速率限制器
    pub fn new(rate_per_sec: u32, capacity: u32) -> Self {
        Self {
            rate_per_sec,
            capacity,
            tokens: AtomicU64::new(capacity as u64),
            last_update: Mutex::new(Instant::now()),
        }
    }

    /// 尝试获取一个令牌
    pub fn try_acquire(&self) -> bool {
        self.update_tokens();
        
        let current = self.tokens.load(Ordering::SeqCst);
        if current > 0 {
            self.tokens.fetch_sub(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// 等待并获取一个令牌
    pub async fn acquire(&self) {
        while !self.try_acquire() {
            let wait_time = 1000 / self.rate_per_sec.max(1);
            tokio::time::sleep(Duration::from_millis(wait_time as u64)).await;
        }
    }

    /// 批量获取令牌
    pub fn try_acquire_n(&self, n: u32) -> bool {
        if n > self.capacity {
            return false;
        }
        
        self.update_tokens();
        
        let current = self.tokens.load(Ordering::SeqCst);
        if current >= n as u64 {
            self.tokens.fetch_sub(n as u64, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// 更新令牌数
    fn update_tokens(&self) {
        let mut last = self.last_update.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(*last);
        
        if elapsed > Duration::from_millis(10) {
            let new_tokens = (elapsed.as_secs_f64() * self.rate_per_sec as f64) as u64;
            let current = self.tokens.load(Ordering::SeqCst);
            let updated = (current + new_tokens).min(self.capacity as u64);
            self.tokens.store(updated, Ordering::SeqCst);
            *last = now;
        }
    }

    /// 获取当前可用令牌数
    pub fn available_tokens(&self) -> u64 {
        self.update_tokens();
        self.tokens.load(Ordering::SeqCst)
    }
}

/// 自适应限流器 - 根据系统负载动态调整
pub struct AdaptiveLimiter {
    /// 基础背压控制
    backpressure: Backpressure,
    /// 速率限制
    rate_limiter: RateLimiter,
    /// 目标延迟（毫秒）
    target_latency_ms: u64,
    /// 当前最大并发
    current_max_concurrent: AtomicUsize,
    /// 最小并发
    min_concurrent: usize,
    /// 最大并发
    max_concurrent: usize,
    /// 采样窗口
    latency_samples: Mutex<VecDeque<u64>>,
    /// 采样窗口大小
    sample_window_size: usize,
}

impl AdaptiveLimiter {
    /// 创建新的自适应限流器
    pub fn new(
        initial_concurrent: usize,
        min_concurrent: usize,
        max_concurrent: usize,
        rate_per_sec: u32,
        target_latency_ms: u64,
    ) -> Self {
        let actual_initial = initial_concurrent.clamp(min_concurrent, max_concurrent);
        
        Self {
            backpressure: Backpressure::new(actual_initial),
            rate_limiter: RateLimiter::new(rate_per_sec, rate_per_sec * 2),
            target_latency_ms,
            current_max_concurrent: AtomicUsize::new(actual_initial),
            min_concurrent,
            max_concurrent,
            latency_samples: Mutex::new(VecDeque::with_capacity(100)),
            sample_window_size: 100,
        }
    }

    /// 获取执行许可
    pub async fn acquire(&self) -> AdaptivePermit {
        // 先检查速率限制
        self.rate_limiter.acquire().await;
        
        // 再获取并发许可
        let permit = self.backpressure.acquire().await;
        
        AdaptivePermit {
            inner: permit,
            limiter: self,
        }
    }

    /// 尝试获取执行许可
    pub fn try_acquire(&self) -> Option<AdaptivePermit> {
        if !self.rate_limiter.try_acquire() {
            return None;
        }
        
        self.backpressure.try_acquire().map(|permit| AdaptivePermit {
            inner: permit,
            limiter: self,
        })
    }

    /// 报告延迟样本（用于自适应调整）
    pub fn report_latency(&self, latency_ms: u64) {
        let mut samples = self.latency_samples.lock().unwrap();
        
        if samples.len() >= self.sample_window_size {
            samples.pop_front();
        }
        samples.push_back(latency_ms);
        
        // 检查是否需要调整
        self.maybe_adjust();
    }

    /// 自适应调整
    fn maybe_adjust(&self) {
        let samples = self.latency_samples.lock().unwrap();
        
        if samples.len() < 10 {
            return; // 样本不足
        }
        
        let avg_latency: u64 = samples.iter().sum::<u64>() / samples.len() as u64;
        let current = self.current_max_concurrent.load(Ordering::SeqCst);
        
        if avg_latency > self.target_latency_ms * 2 {
            // 延迟过高，降低并发
            let new_concurrent = (current / 2).max(self.min_concurrent);
            if new_concurrent < current {
                self.current_max_concurrent.store(new_concurrent, Ordering::SeqCst);
                warn!(
                    "Adaptive limiter: reducing concurrency from {} to {} (avg latency: {}ms)",
                    current, new_concurrent, avg_latency
                );
            }
        } else if avg_latency < self.target_latency_ms / 2 && current < self.max_concurrent {
            // 延迟较低，可以增加并发
            let new_concurrent = (current + 1).min(self.max_concurrent);
            self.current_max_concurrent.store(new_concurrent, Ordering::SeqCst);
            debug!(
                "Adaptive limiter: increasing concurrency to {} (avg latency: {}ms)",
                new_concurrent, avg_latency
            );
        }
    }

    /// 获取当前配置的最大并发
    pub fn current_max_concurrent(&self) -> usize {
        self.current_max_concurrent.load(Ordering::SeqCst)
    }
}

/// 自适应限流许可
pub struct AdaptivePermit<'a> {
    inner: BackpressurePermit,
    limiter: &'a AdaptiveLimiter,
}

impl<'a> AdaptivePermit<'a> {
    /// 释放许可并报告延迟
    pub fn release_with_latency(self, latency_ms: u64) {
        self.limiter.report_latency(latency_ms);
        // Permit 会在 drop 时自动释放
    }

    /// 获取持有时间
    pub fn held_duration(&self) -> Duration {
        self.inner.held_duration()
    }
}

/// 带背压的 Future 包装器
pub struct BackpressureFuture<F> {
    future: F,
    backpressure: Arc<Backpressure>,
    permit: Option<BackpressurePermit>,
}

impl<F, T> Future for BackpressureFuture<F>
where
    F: Future<Output = T> + Unpin,
{
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // 如果没有许可，尝试获取
        if self.permit.is_none() {
            if let Some(permit) = self.backpressure.try_acquire() {
                self.permit = Some(permit);
            } else {
                // 无法获取许可，注册 waker 稍后重试
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        }

        // 有许可，执行 Future
        Pin::new(&mut self.future).poll(cx)
    }
}

/// 背压统计信息
#[derive(Debug, Clone)]
pub struct BackpressureStats {
    pub available_permits: usize,
    pub waiting_count: usize,
    pub active_count: usize,
    pub rejected_count: u64,
    pub total_count: u64,
    pub load_percentage: u8,
}

impl Backpressure {
    /// 获取统计信息
    pub fn stats(&self) -> BackpressureStats {
        BackpressureStats {
            available_permits: self.available_permits(),
            waiting_count: self.waiting_count(),
            active_count: self.active_count(),
            rejected_count: self.rejected_count(),
            total_count: self.total_count(),
            load_percentage: self.load_percentage(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn test_backpressure_acquire() {
        let bp = Backpressure::new(2);
        
        // 应该能获取 2 个许可
        let _permit1 = bp.acquire().await;
        let _permit2 = bp.acquire().await;
        
        assert_eq!(bp.active_count(), 2);
        assert_eq!(bp.available_permits(), 0);
    }

    #[tokio::test]
    async fn test_backpressure_try_acquire() {
        let bp = Backpressure::new(1);
        
        assert!(bp.try_acquire().is_some());
        assert!(bp.try_acquire().is_none()); // 已用完
        
        assert_eq!(bp.rejected_count(), 1);
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let limiter = RateLimiter::new(10, 10); // 每秒10个，容量10
        
        // 应该能获取所有令牌
        for _ in 0..10 {
            assert!(limiter.try_acquire());
        }
        
        // 令牌用完
        assert!(!limiter.try_acquire());
        
        // 等待补充
        sleep(Duration::from_millis(200)).await;
        assert!(limiter.available_tokens() > 0);
    }

    #[tokio::test]
    async fn test_adaptive_limiter() {
        let limiter = AdaptiveLimiter::new(5, 2, 10, 100, 100);
        
        // 获取许可
        let permit = limiter.acquire().await;
        sleep(Duration::from_millis(10)).await;
        
        // 报告延迟
        permit.release_with_latency(10);
        
        assert_eq!(limiter.current_max_concurrent(), 5);
    }

    #[tokio::test]
    async fn test_backpressure_stats() {
        let bp = Backpressure::new(5);
        
        let _permit = bp.acquire().await;
        
        let stats = bp.stats();
        assert_eq!(stats.active_count, 1);
        assert_eq!(stats.available_permits, 4);
        assert_eq!(stats.total_count, 1);
    }

    #[tokio::test]
    async fn test_backpressure_load_percentage() {
        let bp = Backpressure::new(10);
        
        assert_eq!(bp.load_percentage(), 0);
        
        let _permit1 = bp.acquire().await;
        let _permit2 = bp.acquire().await;
        let _permit3 = bp.acquire().await;
        
        assert_eq!(bp.load_percentage(), 30);
    }
}
