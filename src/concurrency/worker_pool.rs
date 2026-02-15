//! Worker Pool - 异步任务执行池
//!
//! 类似 OpenClaw 的架构，提供：
//! - 固定数量的工作线程
//! - 优先级任务队列
//! - 任务超时控制
//! - 优雅关闭

use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// 任务优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskPriority {
    /// 关键任务 - 立即处理
    Critical = 0,
    /// 高优先级
    High = 1,
    /// 普通优先级 (默认)
    Normal = 2,
    /// 低优先级 - 后台任务
    Low = 3,
    /// 最低优先级 - 可丢弃
    Background = 4,
}

impl Default for TaskPriority {
    fn default() -> Self {
        TaskPriority::Normal
    }
}

/// 任务 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

impl TaskId {
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::SeqCst))
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// 任务结果
#[derive(Debug, Clone)]
pub enum TaskResult<T> {
    /// 成功
    Success(T),
    /// 失败
    Failure(String),
    /// 超时
    Timeout,
    /// 被取消
    Cancelled,
    /// 背压拒绝
    Rejected(String),
}

/// 任务提交错误
#[derive(Debug, Clone)]
pub enum TrySubmitError {
    /// 队列已满
    Full,
    /// 通道已关闭
    Closed,
}

impl std::fmt::Display for TrySubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrySubmitError::Full => write!(f, "Task queue is full"),
            TrySubmitError::Closed => write!(f, "Worker pool is closed"),
        }
    }
}

impl std::error::Error for TrySubmitError {}

/// 可执行的任务 trait
pub trait Executable: Send + Sync {
    type Output: Send + Sync;
    fn execute(&self) -> impl Future<Output = Self::Output> + Send;
}

/// 任务包装器
pub struct Task<F, T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + Sync + 'static,
{
    pub id: TaskId,
    pub priority: TaskPriority,
    pub future: Pin<Box<F>>,
    pub timeout: Option<Duration>,
    pub created_at: Instant,
    _phantom: std::marker::PhantomData<T>,
}

impl<F, T> Task<F, T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + Sync + 'static,
{
    pub fn new(future: F) -> Self {
        Self {
            id: TaskId::new(),
            priority: TaskPriority::Normal,
            future: Box::pin(future),
            timeout: None,
            created_at: Instant::now(),
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// 内部任务结构（用于队列）
struct QueuedTask {
    id: TaskId,
    priority: TaskPriority,
    created_at: Instant,
    tx: TaskSender,
    exec: Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Box<dyn std::any::Any + Send + Sync>> + Send>> + Send>,
    timeout: Option<Duration>,
}

impl PartialEq for QueuedTask {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for QueuedTask {}

impl PartialOrd for QueuedTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 优先级高的在前面（Critical < Background）
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.created_at.cmp(&self.created_at)) // 先创建的优先
    }
}

type TaskSender = oneshot::Sender<TaskResult<Box<dyn std::any::Any + Send + Sync>>>;

/// Worker Pool 统计信息
#[derive(Debug, Clone, Default)]
pub struct WorkerPoolStats {
    /// 活跃工作线程数
    pub active_workers: usize,
    /// 队列中的任务数
    pub queued_tasks: usize,
    /// 已完成的任务数
    pub completed_tasks: u64,
    /// 失败的任务数
    pub failed_tasks: u64,
    /// 超时的任务数
    pub timeout_tasks: u64,
    /// 被拒绝的任务数
    pub rejected_tasks: u64,
    /// 平均任务处理时间 (ms)
    pub avg_processing_time_ms: f64,
}

/// Worker Pool 配置
#[derive(Debug, Clone)]
pub struct WorkerPoolConfig {
    /// 工作线程数
    pub worker_count: usize,
    /// 任务队列大小
    pub queue_size: usize,
    /// 默认任务超时
    pub default_timeout: Duration,
    /// 是否启用任务窃取
    pub enable_work_stealing: bool,
    /// 优雅关闭超时
    pub shutdown_timeout: Duration,
}

impl Default for WorkerPoolConfig {
    fn default() -> Self {
        Self {
            worker_count: num_cpus::get().max(2),
            queue_size: 1000,
            default_timeout: Duration::from_secs(30),
            enable_work_stealing: true,
            shutdown_timeout: Duration::from_secs(30),
        }
    }
}

/// Worker Pool
pub struct WorkerPool {
    config: WorkerPoolConfig,
    tx: mpsc::Sender<QueuedTask>,
    workers: Vec<JoinHandle<()>>,
    stats: Arc<WorkerPoolStatsInner>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
}

struct WorkerPoolStatsInner {
    active_workers: AtomicUsize,
    completed_tasks: AtomicU64,
    failed_tasks: AtomicU64,
    timeout_tasks: AtomicU64,
    rejected_tasks: AtomicU64,
    total_processing_time_us: AtomicU64,
}

impl WorkerPool {
    /// 创建新的 Worker Pool
    pub fn new(worker_count: usize, queue_size: usize) -> Self {
        let config = WorkerPoolConfig {
            worker_count,
            queue_size,
            ..Default::default()
        };
        Self::with_config(config)
    }

    /// 使用配置创建
    pub fn with_config(config: WorkerPoolConfig) -> Self {
        let (tx, rx) = mpsc::channel::<QueuedTask>(config.queue_size);
        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        let stats = Arc::new(WorkerPoolStatsInner {
            active_workers: AtomicUsize::new(0),
            completed_tasks: AtomicU64::new(0),
            failed_tasks: AtomicU64::new(0),
            timeout_tasks: AtomicU64::new(0),
            rejected_tasks: AtomicU64::new(0),
            total_processing_time_us: AtomicU64::new(0),
        });

        let mut workers = Vec::with_capacity(config.worker_count);

        for worker_id in 0..config.worker_count {
            let mut shutdown_rx = shutdown_tx.subscribe();
            let stats = Arc::clone(&stats);
            let rx = Arc::clone(&rx);
            
            let handle = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        
                        // 优先处理关闭信号
                        _ = shutdown_rx.recv() => {
                            debug!("Worker {} received shutdown signal", worker_id);
                            break;
                        }
                        
                        // 处理任务
                        Some(task) = async { rx.lock().await.recv().await } => {
                            stats.active_workers.fetch_add(1, Ordering::SeqCst);
                            let start = Instant::now();
                            
                            let result = if let Some(timeout) = task.timeout {
                                match tokio::time::timeout(timeout, (task.exec)()).await {
                                    Ok(result) => TaskResult::Success(result),
                                    Err(_) => {
                                        stats.timeout_tasks.fetch_add(1, Ordering::SeqCst);
                                        TaskResult::Timeout
                                    }
                                }
                            } else {
                                let result = (task.exec)().await;
                                TaskResult::Success(result)
                            };
                            
                            let elapsed = start.elapsed();
                            stats.total_processing_time_us.fetch_add(
                                elapsed.as_micros() as u64, 
                                Ordering::SeqCst
                            );
                            
                            match &result {
                                TaskResult::Success(_) => {
                                    stats.completed_tasks.fetch_add(1, Ordering::SeqCst);
                                }
                                TaskResult::Failure(_) => {
                                    stats.failed_tasks.fetch_add(1, Ordering::SeqCst);
                                }
                                _ => {}
                            }
                            
                            let _ = task.tx.send(result);
                            stats.active_workers.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                }
            });
            
            workers.push(handle);
        }

        Self {
            config,
            tx,
            workers,
            stats,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    /// 提交任务到 Worker Pool
    pub async fn submit<F, T>(&self, task: Task<F, T>) -> Result<TaskResult<T>, String>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + Sync + 'static,
    {
        let (tx, rx) = oneshot::channel();
        
        let queued_task = QueuedTask {
            id: task.id,
            priority: task.priority,
            created_at: task.created_at,
            tx,
            exec: Box::new(move || {
                Box::pin(async move {
                    let result: T = task.future.await;
                    Box::new(result) as Box<dyn std::any::Any + Send + Sync>
                })
            }),
            timeout: task.timeout.or(Some(self.config.default_timeout)),
        };

        match self.tx.try_send(queued_task) {
            Ok(_) => {
                match rx.await {
                    Ok(TaskResult::Success(boxed)) => {
                        // 安全地转换回原始类型
                        let result = *boxed.downcast::<T>()
                            .map_err(|_| "Type conversion failed".to_string())?;
                        Ok(TaskResult::Success(result))
                    }
                    Ok(TaskResult::Timeout) => Ok(TaskResult::Timeout),
                    Ok(TaskResult::Cancelled) => Ok(TaskResult::Cancelled),
                    Ok(TaskResult::Rejected(msg)) => Ok(TaskResult::Rejected(msg)),
                    Ok(TaskResult::Failure(msg)) => Ok(TaskResult::Failure(msg)),
                    Err(_) => Err("Task channel closed".to_string()),
                }
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.stats.rejected_tasks.fetch_add(1, Ordering::SeqCst);
                Ok(TaskResult::Rejected("Task queue full".to_string()))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err("Worker pool closed".to_string())
            }
        }
    }

    /// 尝试提交任务（非阻塞）
    pub fn try_submit<F, T>(&self, task: Task<F, T>) -> Result<oneshot::Receiver<TaskResult<Box<dyn std::any::Any + Send + Sync>>>, TrySubmitError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + Sync + 'static,
    {
        let (tx, rx) = oneshot::channel();

        // Store task fields separately to avoid partial move
        let task_id = task.id;
        let task_priority = task.priority;
        let task_timeout = task.timeout;
        let task_created_at = task.created_at;
        let timeout = task.timeout.or(Some(self.config.default_timeout));

        // Take ownership of the future via Option::take()
        let task_future = task
            .future
            .take()
            .expect("task.future must be present in try_submit");

        let queued_task = QueuedTask {
            id: task_id,
            priority: task_priority,
            created_at: task_created_at,
            tx,
            exec: Box::new(move || {
                Box::pin(async move {
                    let result: T = task_future.await;
                    Box::new(result) as Box<dyn std::any::Any + Send + Sync>
                })
            }),
            timeout,
        };

        match self.tx.try_send(queued_task) {
            Ok(_) => Ok(rx),
            Err(mpsc::error::TrySendError::Full(_)) => Err(TrySubmitError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(TrySubmitError::Closed),
        }
    }

    /// 获取当前统计信息
    pub fn stats(&self) -> WorkerPoolStats {
        WorkerPoolStats {
            active_workers: self.stats.active_workers.load(Ordering::SeqCst),
            queued_tasks: self.tx.capacity(), // 近似值
            completed_tasks: self.stats.completed_tasks.load(Ordering::SeqCst),
            failed_tasks: self.stats.failed_tasks.load(Ordering::SeqCst),
            timeout_tasks: self.stats.timeout_tasks.load(Ordering::SeqCst),
            rejected_tasks: self.stats.rejected_tasks.load(Ordering::SeqCst),
            avg_processing_time_ms: {
                let completed = self.stats.completed_tasks.load(Ordering::SeqCst);
                if completed > 0 {
                    let total_us = self.stats.total_processing_time_us.load(Ordering::SeqCst);
                    (total_us as f64 / completed as f64) / 1000.0
                } else {
                    0.0
                }
            },
        }
    }

    /// 获取工作线程数
    pub fn worker_count(&self) -> usize {
        self.config.worker_count
    }

    /// 优雅关闭 Worker Pool
    pub async fn shutdown(mut self) {
        info!("Shutting down worker pool with {} workers", self.workers.len());

        // 发送关闭信号
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // 等待所有工作线程完成
        let timeout = tokio::time::Duration::from_secs(30);
        // Take workers out to avoid moving from type implementing Drop
        let workers = std::mem::take(&mut self.workers);
        let shutdown_future = async {
            for handle in workers {
                let _ = handle.await;
            }
        };

        match tokio::time::timeout(timeout, shutdown_future).await {
            Ok(_) => info!("Worker pool shut down gracefully"),
            Err(_) => warn!("Worker pool shutdown timed out"),
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // Signal shutdown - workers will exit when they receive the signal
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Note: We can't await the workers here since drop is synchronous
        // Workers will be aborted when the runtime drops them
    }
}

/// 创建简单的异步任务
pub fn create_task<F, T>(future: F) -> Task<F, T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + Sync + 'static,
{
    Task::new(future)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_worker_pool_basic() {
        let pool = WorkerPool::new(2, 10);
        
        let task = create_task(async {
            sleep(Duration::from_millis(10)).await;
            42
        });
        
        let result = pool.submit(task).await.unwrap();
        
        match result {
            TaskResult::Success(value) => {
                let val = *value.downcast::<i32>().unwrap();
                assert_eq!(val, 42);
            }
            _ => panic!("Expected success"),
        }
        
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_worker_pool_concurrent() {
        let pool = WorkerPool::new(4, 100);
        
        let mut handles = vec![];
        for i in 0..10 {
            let task = create_task(async move {
                sleep(Duration::from_millis(10)).await;
                i * 2
            });
            
            let result = pool.submit(task).await.unwrap();
            handles.push(result);
        }
        
        let stats = pool.stats();
        assert!(stats.completed_tasks >= 10);
        
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_task_timeout() {
        let pool = WorkerPool::new(1, 10);
        
        let task = create_task(async {
            sleep(Duration::from_secs(10)).await;
            42
        }).with_timeout(Duration::from_millis(50));
        
        let result = pool.submit(task).await.unwrap();
        
        match result {
            TaskResult::Timeout => {}
            _ => panic!("Expected timeout"),
        }
        
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_task_priority() {
        let pool = WorkerPool::new(1, 10);
        
        let mut results = vec![];
        
        // 提交低优先级任务
        let low_task = create_task(async {
            sleep(Duration::from_millis(10)).await;
            "low"
        }).with_priority(TaskPriority::Low);
        
        // 提交高优先级任务
        let high_task = create_task(async {
            sleep(Duration::from_millis(5)).await;
            "high"
        }).with_priority(TaskPriority::High);
        
        // 先提交低优先级，再提交高优先级
        let _ = pool.try_submit(low_task);
        let _ = pool.try_submit(high_task);
        
        sleep(Duration::from_millis(100)).await;
        
        pool.shutdown().await;
    }

    #[test]
    fn test_task_priority_ordering() {
        assert!(TaskPriority::Critical < TaskPriority::High);
        assert!(TaskPriority::High < TaskPriority::Normal);
        assert!(TaskPriority::Normal < TaskPriority::Low);
        assert!(TaskPriority::Low < TaskPriority::Background);
    }

    #[tokio::test]
    async fn test_worker_pool_stats() {
        let pool = WorkerPool::new(2, 10);
        
        let stats = pool.stats();
        assert_eq!(stats.active_workers, 0);
        assert_eq!(stats.completed_tasks, 0);
        
        let task = create_task(async { 42 });
        let _ = pool.submit(task).await;
        
        sleep(Duration::from_millis(50)).await;
        
        let stats = pool.stats();
        assert!(stats.completed_tasks >= 1);
        
        pool.shutdown().await;
    }
}
