//! Circuit Breaker - 熔断器
//!
//! 防止级联故障，实现快速失败：
//! - 三种状态：Closed, Open, HalfOpen
//! - 基于失败率的自动熔断
//! - 可配置的恢复策略
//! - 半开状态的探测机制

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, error, info, trace, warn};

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CircuitState {
    /// 关闭状态 - 正常服务
    Closed,
    /// 开启状态 - 熔断，拒绝请求
    Open,
    /// 半开状态 - 允许部分请求测试
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "Closed"),
            CircuitState::Open => write!(f, "Open"),
            CircuitState::HalfOpen => write!(f, "HalfOpen"),
        }
    }
}

/// 熔断器配置
#[derive(Debug, Clone)]
pub struct CircuitConfig {
    /// 失败阈值 - 触发熔断的失败次数
    pub failure_threshold: u32,
    /// 成功阈值 - 半开状态恢复所需的连续成功次数
    pub success_threshold: u32,
    /// 熔断超时时间
    pub timeout_duration: Duration,
    /// 半开状态允许的请求比例 (0.0-1.0)
    pub half_open_max_ratio: f64,
    /// 统计窗口大小
    pub stats_window_size: usize,
    /// 是否启用半开状态
    pub enable_half_open: bool,
    /// 最小调用次数才开始统计失败率
    pub min_calls_before_stat: u32,
    /// 失败率阈值 (0.0-1.0) - 超过此值触发熔断
    pub failure_rate_threshold: f64,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 3,
            timeout_duration: Duration::from_secs(60),
            half_open_max_ratio: 0.1,
            stats_window_size: 100,
            enable_half_open: true,
            min_calls_before_stat: 10,
            failure_rate_threshold: 0.5,
        }
    }
}

impl CircuitConfig {
    /// 快速失败配置 - 立即熔断
    pub fn fast_fail() -> Self {
        Self {
            failure_threshold: 1,
            timeout_duration: Duration::from_secs(30),
            ..Default::default()
        }
    }

    /// 宽松配置 - 容忍更多失败
    pub fn lenient() -> Self {
        Self {
            failure_threshold: 10,
            failure_rate_threshold: 0.8,
            timeout_duration: Duration::from_secs(120),
            ..Default::default()
        }
    }

    /// 严格配置 - 快速熔断
    pub fn strict() -> Self {
        Self {
            failure_threshold: 3,
            failure_rate_threshold: 0.3,
            timeout_duration: Duration::from_secs(30),
            success_threshold: 5,
            ..Default::default()
        }
    }
}

/// 调用结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallResult {
    /// 成功
    Success,
    /// 失败
    Failure,
    /// 超时
    Timeout,
    /// 被拒绝（熔断状态）
    Rejected,
}

/// 统计窗口中的单个记录
#[derive(Debug, Clone, Copy)]
struct StatRecord {
    result: CallResult,
    timestamp: Instant,
}

/// 熔断器统计
struct CircuitStats {
    /// 统计窗口
    records: Vec<StatRecord>,
    /// 窗口大小
    window_size: usize,
    /// 连续失败次数
    consecutive_failures: u32,
    /// 连续成功次数
    consecutive_successes: u32,
    /// 总调用次数
    total_calls: u64,
}

impl CircuitStats {
    fn new(window_size: usize) -> Self {
        Self {
            records: Vec::with_capacity(window_size),
            window_size,
            consecutive_failures: 0,
            consecutive_successes: 0,
            total_calls: 0,
        }
    }

    /// 记录调用结果
    fn record(&mut self, result: CallResult) {
        let now = Instant::now();
        
        // 保持窗口大小
        if self.records.len() >= self.window_size {
            self.records.remove(0);
        }
        
        self.records.push(StatRecord {
            result,
            timestamp: now,
        });
        
        self.total_calls += 1;
        
        // 更新连续计数
        match result {
            CallResult::Success => {
                self.consecutive_successes += 1;
                self.consecutive_failures = 0;
            }
            CallResult::Failure | CallResult::Timeout => {
                self.consecutive_failures += 1;
                self.consecutive_successes = 0;
            }
            _ => {}
        }
    }

    /// 计算失败率
    fn failure_rate(&self) -> f64 {
        let total = self.records.len();
        if total == 0 {
            return 0.0;
        }
        
        let failures = self.records
            .iter()
            .filter(|r| matches!(r.result, CallResult::Failure | CallResult::Timeout))
            .count();
        
        failures as f64 / total as f64
    }

    /// 获取连续失败次数
    fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// 获取连续成功次数
    fn consecutive_successes(&self) -> u32 {
        self.consecutive_successes
    }

    /// 重置
    fn reset(&mut self) {
        self.records.clear();
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
    }
}

/// 熔断器
pub struct CircuitBreaker {
    /// 配置
    config: CircuitConfig,
    /// 当前状态
    state: RwLock<CircuitState>,
    /// 统计信息
    stats: Mutex<CircuitStats>,
    /// 上次状态改变时间
    last_state_change: Mutex<Instant>,
    /// 半开状态计数器
    half_open_count: AtomicUsize,
    /// 半开状态请求总数
    half_open_total: AtomicUsize,
    /// 名称（用于日志）
    name: String,
    /// 状态变更回调
    state_change_handlers: Mutex<Vec<Box<dyn Fn(CircuitState, CircuitState) + Send + Sync>>>,
}

impl CircuitBreaker {
    /// 创建新的熔断器
    pub fn new(config: CircuitConfig) -> Self {
        Self::with_name("default", config)
    }

    /// 指定名称创建
    pub fn with_name(name: impl Into<String>, config: CircuitConfig) -> Self {
        Self {
            config,
            state: RwLock::new(CircuitState::Closed),
            stats: Mutex::new(CircuitStats::new(100)),
            last_state_change: Mutex::new(Instant::now()),
            half_open_count: AtomicUsize::new(0),
            half_open_total: AtomicUsize::new(0),
            name: name.into(),
            state_change_handlers: Mutex::new(Vec::new()),
        }
    }

    /// 获取当前状态
    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap()
    }

    /// 检查是否允许请求通过
    pub fn allow_request(&self) -> bool {
        let state = self.state();
        
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // 检查是否可以进入半开状态
                if self.should_attempt_reset() {
                    if self.config.enable_half_open {
                        self.transition_to(CircuitState::HalfOpen);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // 限制半开状态的请求数量
                let total = self.half_open_total.fetch_add(1, Ordering::SeqCst) + 1;
                let allowed = (total as f64 * self.config.half_open_max_ratio).ceil() as usize;
                let current = self.half_open_count.load(Ordering::SeqCst);
                
                if current < allowed {
                    self.half_open_count.fetch_add(1, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// 记录成功
    pub fn record_success(&self) {
        let state = self.state();
        
        {
            let mut stats = self.stats.lock().unwrap();
            stats.record(CallResult::Success);
        }
        
        match state {
            CircuitState::HalfOpen => {
                let successes = self.stats.lock().unwrap().consecutive_successes();
                if successes >= self.config.success_threshold {
                    info!(
                        "Circuit breaker '{}' recovered after {} consecutive successes",
                        self.name, successes
                    );
                    self.transition_to(CircuitState::Closed);
                }
            }
            _ => {}
        }
    }

    /// 记录失败
    pub fn record_failure(&self) {
        let state = self.state();
        
        {
            let mut stats = self.stats.lock().unwrap();
            stats.record(CallResult::Failure);
        }
        
        match state {
            CircuitState::Closed => {
                self.check_and_trip();
            }
            CircuitState::HalfOpen => {
                // 半开状态任何失败都重新熔断
                warn!(
                    "Circuit breaker '{}' re-tripped after failure in half-open state",
                    self.name
                );
                self.transition_to(CircuitState::Open);
            }
            _ => {}
        }
    }

    /// 记录超时
    pub fn record_timeout(&self) {
        // 超时视为失败
        self.record_failure();
    }

    /// 记录被拒绝
    pub fn record_rejected(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.record(CallResult::Rejected);
    }

    /// 强制熔断（手动控制）
    pub fn force_open(&self) {
        warn!("Circuit breaker '{}' manually opened", self.name);
        self.transition_to(CircuitState::Open);
    }

    /// 强制关闭（手动控制）
    pub fn force_close(&self) {
        info!("Circuit breaker '{}' manually closed", self.name);
        self.transition_to(CircuitState::Closed);
    }

    /// 注册状态变更处理器
    pub fn on_state_change<F>(&self, handler: F)
    where
        F: Fn(CircuitState, CircuitState) + Send + Sync + 'static,
    {
        self.state_change_handlers.lock().unwrap().push(Box::new(handler));
    }

    /// 获取统计信息
    pub fn stats(&self) -> CircuitBreakerStats {
        let state = self.state();
        let stats = self.stats.lock().unwrap();
        
        CircuitBreakerStats {
            state,
            failure_rate: stats.failure_rate(),
            consecutive_failures: stats.consecutive_failures(),
            consecutive_successes: stats.consecutive_successes(),
            total_calls: stats.total_calls,
            time_in_current_state: self.last_state_change.lock().unwrap().elapsed(),
        }
    }

    /// 重置熔断器
    pub fn reset(&self) {
        let mut stats = self.stats.lock().unwrap();
        stats.reset();
        drop(stats);
        
        self.half_open_count.store(0, Ordering::SeqCst);
        self.half_open_total.store(0, Ordering::SeqCst);
        
        self.transition_to(CircuitState::Closed);
    }

    /// 检查是否应该尝试重置
    fn should_attempt_reset(&self) -> bool {
        let last_change = *self.last_state_change.lock().unwrap();
        last_change.elapsed() >= self.config.timeout_duration
    }

    /// 检查并触发熔断
    fn check_and_trip(&self) {
        let stats = self.stats.lock().unwrap();
        
        // 检查连续失败
        let consecutive_failures = stats.consecutive_failures();
        let failure_rate = stats.failure_rate();
        let total_calls = stats.total_calls;
        
        let should_trip = consecutive_failures >= self.config.failure_threshold
            || (total_calls >= self.config.min_calls_before_stat as u64 
                && failure_rate >= self.config.failure_rate_threshold);
        
        drop(stats);
        
        if should_trip {
            error!(
                "Circuit breaker '{}' tripped: {} consecutive failures, {:.2}% failure rate",
                self.name, consecutive_failures, failure_rate * 100.0
            );
            self.transition_to(CircuitState::Open);
        }
    }

    /// 状态转换
    fn transition_to(&self, new_state: CircuitState) {
        let old_state = {
            let mut state = self.state.write().unwrap();
            let old = *state;
            *state = new_state;
            old
        };
        
        if old_state != new_state {
            *self.last_state_change.lock().unwrap() = Instant::now();
            
            // 重置半开计数器
            if new_state != CircuitState::HalfOpen {
                self.half_open_count.store(0, Ordering::SeqCst);
                self.half_open_total.store(0, Ordering::SeqCst);
            }
            
            // 如果切换到 Closed，重置统计数据
            if new_state == CircuitState::Closed {
                self.stats.lock().unwrap().reset();
            }
            
            info!(
                "Circuit breaker '{}' state changed: {} -> {}",
                self.name, old_state, new_state
            );
            
            // 触发回调
            for handler in self.state_change_handlers.lock().unwrap().iter() {
                handler(old_state, new_state);
            }
        }
    }
}

impl Clone for CircuitBreaker {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            state: RwLock::new(self.state()),
            stats: Mutex::new(CircuitStats::new(self.config.stats_window_size)),
            last_state_change: Mutex::new(Instant::now()),
            half_open_count: AtomicUsize::new(0),
            half_open_total: AtomicUsize::new(0),
            name: format!("{}_clone", self.name),
            state_change_handlers: Mutex::new(Vec::new()),
        }
    }
}

/// 熔断器统计信息
#[derive(Debug, Clone)]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub failure_rate: f64,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub total_calls: u64,
    pub time_in_current_state: Duration,
}

/// 保护 Future 的熔断器包装器
pub struct CircuitBreakerFuture<F> {
    future: F,
    breaker: Arc<CircuitBreaker>,
    started: bool,
}

impl<F, T> CircuitBreakerFuture<F>
where
    F: Future<Output = Result<T, anyhow::Error>>,
{
    pub fn new(future: F, breaker: Arc<CircuitBreaker>) -> Self {
        Self {
            future,
            breaker,
            started: false,
        }
    }
}

impl<F, T> Future for CircuitBreakerFuture<F>
where
    F: Future<Output = Result<T, anyhow::Error>> + Unpin,
{
    type Output = Result<T, CircuitError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.started {
            if !self.breaker.allow_request() {
                return Poll::Ready(Err(CircuitError::Open));
            }
            self.started = true;
        }

        match Pin::new(&mut self.future).poll(cx) {
            Poll::Ready(Ok(result)) => {
                self.breaker.record_success();
                Poll::Ready(Ok(result))
            }
            Poll::Ready(Err(e)) => {
                self.breaker.record_failure();
                Poll::Ready(Err(CircuitError::Inner(std::sync::Arc::new(e))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// 熔断器错误
#[derive(Debug, Clone)]
pub enum CircuitError {
    /// 熔断器开启，请求被拒绝
    Open,
    /// 内部错误
    Inner(std::sync::Arc<anyhow::Error>),
    /// 超时
    Timeout,
}

impl std::fmt::Display for CircuitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitError::Open => write!(f, "Circuit breaker is open"),
            CircuitError::Inner(e) => write!(f, "Inner error: {}", e),
            CircuitError::Timeout => write!(f, "Circuit breaker timeout"),
        }
    }
}

impl std::error::Error for CircuitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CircuitError::Inner(e) => Some(e.root_cause()),
            _ => None,
        }
    }
}

/// 熔断器组 - 管理多个熔断器
pub struct CircuitBreakerGroup {
    breakers: Mutex<HashMap<String, Arc<CircuitBreaker>>>,
    default_config: CircuitConfig,
}

impl CircuitBreakerGroup {
    /// 创建新的熔断器组
    pub fn new() -> Self {
        Self::with_config(CircuitConfig::default())
    }

    /// 使用默认配置创建
    pub fn with_config(default_config: CircuitConfig) -> Self {
        Self {
            breakers: Mutex::new(HashMap::new()),
            default_config,
        }
    }

    /// 获取或创建熔断器
    pub fn get_or_create(&self, name: &str) -> Arc<CircuitBreaker> {
        let mut breakers = self.breakers.lock().unwrap();
        
        if let Some(breaker) = breakers.get(name) {
            Arc::clone(breaker)
        } else {
            let breaker = Arc::new(CircuitBreaker::with_name(name, self.default_config.clone()));
            breakers.insert(name.to_string(), Arc::clone(&breaker));
            breaker
        }
    }

    /// 获取熔断器（如果存在）
    pub fn get(&self, name: &str) -> Option<Arc<CircuitBreaker>> {
        self.breakers.lock().unwrap().get(name).map(Arc::clone)
    }

    /// 创建带自定义配置的熔断器
    pub fn create(&self, name: &str, config: CircuitConfig) -> Arc<CircuitBreaker> {
        let breaker = Arc::new(CircuitBreaker::with_name(name, config));
        self.breakers.lock().unwrap().insert(name.to_string(), Arc::clone(&breaker));
        breaker
    }

    /// 移除熔断器
    pub fn remove(&self, name: &str) -> Option<Arc<CircuitBreaker>> {
        self.breakers.lock().unwrap().remove(name)
    }

    /// 获取所有熔断器的状态
    pub fn all_stats(&self) -> HashMap<String, CircuitBreakerStats> {
        let breakers = self.breakers.lock().unwrap();
        breakers
            .iter()
            .map(|(name, breaker)| (name.clone(), breaker.stats()))
            .collect()
    }

    /// 重置所有熔断器
    pub fn reset_all(&self) {
        let breakers = self.breakers.lock().unwrap();
        for (_, breaker) in breakers.iter() {
            breaker.reset();
        }
    }
}

impl Default for CircuitBreakerGroup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_initial_state() {
        let cb = CircuitBreaker::new(CircuitConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_trip() {
        let config = CircuitConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);
        
        // 记录 3 次失败
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_recovery() {
        let config = CircuitConfig {
            failure_threshold: 2,
            success_threshold: 2,
            enable_half_open: true,
            timeout_duration: Duration::from_millis(1),
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);
        
        // 熔断
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        
        // 等待超时
        std::thread::sleep(Duration::from_millis(10));
        
        // 应该可以进入半开状态
        assert!(cb.allow_request());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        
        // 记录成功恢复
        cb.record_success();
        cb.record_success();
        
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_stats() {
        let cb = CircuitBreaker::new(CircuitConfig::default());
        
        cb.record_success();
        cb.record_success();
        cb.record_failure();
        
        let stats = cb.stats();
        assert_eq!(stats.state, CircuitState::Closed);
        assert_eq!(stats.consecutive_successes, 1);
        assert_eq!(stats.consecutive_failures, 1);
        assert_eq!(stats.total_calls, 3);
    }

    #[test]
    fn test_circuit_breaker_force_control() {
        let cb = CircuitBreaker::new(CircuitConfig::default());
        
        cb.force_open();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow_request());
        
        cb.force_close();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_failure_rate() {
        let config = CircuitConfig {
            min_calls_before_stat: 5,
            failure_rate_threshold: 0.5,
            failure_threshold: 100, // 很高，不会触发
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);
        
        // 记录 10 次，8 次失败（80% 失败率）
        for i in 0..10 {
            if i < 8 {
                cb.record_failure();
            } else {
                cb.record_success();
            }
        }
        
        // 失败率超过 50%，应该熔断
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_group() {
        let group = CircuitBreakerGroup::new();
        
        let cb1 = group.get_or_create("service1");
        let cb2 = group.get_or_create("service2");
        let cb1_again = group.get_or_create("service1");
        
        assert!(Arc::ptr_eq(&cb1, &cb1_again));
        assert!(!Arc::ptr_eq(&cb1, &cb2));
        
        let stats = group.all_stats();
        assert_eq!(stats.len(), 2);
    }

    #[test]
    fn test_circuit_config_presets() {
        let fast = CircuitConfig::fast_fail();
        assert_eq!(fast.failure_threshold, 1);
        
        let lenient = CircuitConfig::lenient();
        assert_eq!(lenient.failure_threshold, 10);
        assert_eq!(lenient.failure_rate_threshold, 0.8);
        
        let strict = CircuitConfig::strict();
        assert_eq!(strict.failure_threshold, 3);
        assert_eq!(strict.failure_rate_threshold, 0.3);
    }

    #[test]
    fn test_state_display() {
        assert_eq!(format!("{}", CircuitState::Closed), "Closed");
        assert_eq!(format!("{}", CircuitState::Open), "Open");
        assert_eq!(format!("{}", CircuitState::HalfOpen), "HalfOpen");
    }
}
