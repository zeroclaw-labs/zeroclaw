//! Channel 并发处理集成
//!
//! 将 Worker Pool、背压、去重、熔断器集成到 Channel 消息处理流程中：
//! - 消息队列处理
//! - 自动去重
//! - 熔断保护
//! - 背压控制

use crate::channels::traits::{Channel, ChannelMessage};
use crate::concurrency::{
    Backpressure, BackpressureStats, CircuitBreaker, CircuitBreakerGroup, CircuitConfig,
    CircuitState, DedupKey, Deduplicator, Task, TaskPriority, TaskResult, WorkerPool,
    WorkerPoolStats,
};
use crate::providers::Provider;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, trace, warn};

/// 消息处理器配置
#[derive(Debug, Clone)]
pub struct MessageProcessorConfig {
    /// Worker Pool 大小
    pub worker_pool_size: usize,
    /// 任务队列大小
    pub task_queue_size: usize,
    /// 最大并发请求数
    pub max_concurrent_requests: usize,
    /// 去重 TTL
    pub dedup_ttl: Duration,
    /// 熔断器配置
    pub circuit_config: CircuitConfig,
    /// 是否启用去重
    pub enable_dedup: bool,
    /// 是否启用熔断器
    pub enable_circuit_breaker: bool,
    /// 是否启用背压
    pub enable_backpressure: bool,
    /// 消息处理超时
    pub processing_timeout: Duration,
}

impl Default for MessageProcessorConfig {
    fn default() -> Self {
        Self {
            worker_pool_size: 4,
            task_queue_size: 100,
            max_concurrent_requests: 10,
            dedup_ttl: Duration::from_secs(60),
            circuit_config: CircuitConfig::default(),
            enable_dedup: true,
            enable_circuit_breaker: true,
            enable_backpressure: true,
            processing_timeout: Duration::from_secs(30),
        }
    }
}

/// 消息处理结果
#[derive(Debug, Clone)]
pub enum MessageResult {
    /// 成功
    Success(String),
    /// 失败
    Failed(String),
    /// 超时
    Timeout,
    /// 重复消息
    Duplicate,
    /// 被拒绝（背压或熔断）
    Rejected(String),
}

/// 消息处理器 - 集成所有并发控制功能
pub struct MessageProcessor {
    config: MessageProcessorConfig,
    worker_pool: WorkerPool,
    deduplicator: Option<Deduplicator>,
    circuit_breaker: Option<CircuitBreaker>,
    backpressure: Option<Backpressure>,
    /// 通道响应发送器
    response_tx: mpsc::Sender<(String, ChannelMessage, MessageResult)>,
}

impl MessageProcessor {
    /// 创建新的消息处理器
    pub fn new(config: MessageProcessorConfig) -> (Self, mpsc::Receiver<(String, ChannelMessage, MessageResult)>) {
        let (response_tx, response_rx) = mpsc::channel(1000);
        
        let processor = Self {
            worker_pool: WorkerPool::new(config.worker_pool_size, config.task_queue_size),
            deduplicator: if config.enable_dedup {
                Some(Deduplicator::new(config.dedup_ttl))
            } else {
                None
            },
            circuit_breaker: if config.enable_circuit_breaker {
                Some(CircuitBreaker::new(config.circuit_config.clone()))
            } else {
                None
            },
            backpressure: if config.enable_backpressure {
                Some(Backpressure::new(config.max_concurrent_requests))
            } else {
                None
            },
            config,
            response_tx,
        };
        
        (processor, response_rx)
    }

    /// 处理消息
    pub async fn process_message<F, Fut>(
        &self,
        channel: Arc<dyn Channel>,
        message: ChannelMessage,
        processor: F,
    ) -> MessageResult
    where
        F: FnOnce(ChannelMessage) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, anyhow::Error>> + Send + 'static,
    {
        // 1. 检查去重
        if let Some(ref dedup) = self.deduplicator {
            let dedup_key = self.create_dedup_key(&message);
            if dedup.check_and_update(&dedup_key) {
                trace!("Duplicate message detected: {}", message.id);
                return MessageResult::Duplicate;
            }
        }

        // 2. 检查熔断器
        if let Some(ref cb) = self.circuit_breaker {
            if !cb.allow_request() {
                cb.record_rejected();
                return MessageResult::Rejected("Circuit breaker is open".to_string());
            }
        }

        // 3. 获取背压许可
        let _permit = if let Some(ref bp) = self.backpressure {
            match bp.try_acquire() {
                Some(permit) => Some(permit),
                None => {
                    return MessageResult::Rejected("Backpressure limit reached".to_string());
                }
            }
        } else {
            None
        };

        // 4. 提交任务到 Worker Pool
        let channel_name = channel.name().to_string();
        let response_tx = self.response_tx.clone();
        let circuit_breaker = self.circuit_breaker.clone();
        let timeout = self.config.processing_timeout;

        // Clone message for use after the move
        let message_for_priority = message.clone();
        let task = Task::new(async move {
            let start = Instant::now();

            let result = match tokio::time::timeout(timeout, processor(message.clone())).await {
                Ok(Ok(response)) => {
                    // 记录成功
                    if let Some(ref cb) = circuit_breaker {
                        cb.record_success();
                    }
                    MessageResult::Success(response)
                }
                Ok(Err(e)) => {
                    // 记录失败
                    if let Some(ref cb) = circuit_breaker {
                        cb.record_failure();
                    }
                    MessageResult::Failed(e.to_string())
                }
                Err(_) => {
                    // 超时
                    if let Some(ref cb) = circuit_breaker {
                        cb.record_timeout();
                    }
                    MessageResult::Timeout
                }
            };

            let elapsed = start.elapsed();
            debug!(
                "Message {} processed in {:?}: {:?}",
                message.id, elapsed, result
            );

            // 发送响应
            let _ = response_tx.send((channel_name, message, result)).await;
        })
        .with_priority(self.priority_from_channel(&message_for_priority.channel))
        .with_timeout(timeout + Duration::from_secs(5));

        // 提交任务（不等待结果，通过 response_rx 接收）
        match self.worker_pool.submit(task).await {
            Ok(_) => {
                // 任务已提交，异步处理
                MessageResult::Success("Processing".to_string())
            }
            Err(e) => MessageResult::Failed(format!("Failed to submit task: {}", e)),
            Ok(TaskResult::Rejected(msg)) => MessageResult::Rejected(msg),
            _ => MessageResult::Failed("Unknown error".to_string()),
        }
    }

    /// 尝试处理消息（非阻塞）
    pub fn try_process_message<F, Fut>(
        &self,
        channel: Arc<dyn Channel>,
        message: ChannelMessage,
        processor: F,
    ) -> Option<MessageResult>
    where
        F: FnOnce(ChannelMessage) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, anyhow::Error>> + Send + 'static,
    {
        // 快速检查
        if let Some(ref dedup) = self.deduplicator {
            let dedup_key = self.create_dedup_key(&message);
            if dedup.check_and_update(&dedup_key) {
                return Some(MessageResult::Duplicate);
            }
        }

        if let Some(ref cb) = self.circuit_breaker {
            if !cb.allow_request() {
                return Some(MessageResult::Rejected("Circuit breaker open".to_string()));
            }
        }

        if let Some(ref bp) = self.backpressure {
            if bp.available_permits() == 0 {
                return Some(MessageResult::Rejected("Backpressure limit".to_string()));
            }
        }

        // 异步提交
        let channel_name = channel.name().to_string();
        let response_tx = self.response_tx.clone();
        let circuit_breaker = self.circuit_breaker.clone();
        let timeout = self.config.processing_timeout;

        let task = Task::new(async move {
            let _start = Instant::now();
            
            let result = match tokio::time::timeout(timeout, processor(message.clone())).await {
                Ok(Ok(response)) => {
                    if let Some(ref cb) = circuit_breaker {
                        cb.record_success();
                    }
                    MessageResult::Success(response)
                }
                Ok(Err(e)) => {
                    if let Some(ref cb) = circuit_breaker {
                        cb.record_failure();
                    }
                    MessageResult::Failed(e.to_string())
                }
                Err(_) => {
                    if let Some(ref cb) = circuit_breaker {
                        cb.record_timeout();
                    }
                    MessageResult::Timeout
                }
            };

            let _ = response_tx.send((channel_name, message, result)).await;
        })
        .with_timeout(timeout + Duration::from_secs(5));

        match self.worker_pool.try_submit(task) {
            Ok(_) => Some(MessageResult::Success("Queued".to_string())),
            Err(_) => Some(MessageResult::Rejected("Task queue full".to_string())),
        }
    }

    /// 获取统计信息
    pub fn stats(&self) -> ProcessorStats {
        ProcessorStats {
            worker_pool: self.worker_pool.stats(),
            backpressure: self.backpressure.as_ref().map(|bp| bp.stats()),
            circuit_breaker: self.circuit_breaker.as_ref().map(|cb| cb.stats()),
            deduplicator: self.deduplicator.as_ref().map(|d| d.stats()),
        }
    }

    /// 优雅关闭
    pub async fn shutdown(self) {
        info!("Shutting down message processor...");
        self.worker_pool.shutdown().await;
        info!("Message processor shut down complete");
    }

    /// 创建去重键
    fn create_dedup_key(&self, message: &ChannelMessage) -> DedupKey {
        DedupKey::combine(vec![
            message.channel.clone(),
            message.sender.clone(),
            message.content.clone(),
        ])
    }

    /// 根据通道确定优先级
    fn priority_from_channel(&self, channel: &str) -> TaskPriority {
        match channel {
            "telegram" | "discord" | "slack" => TaskPriority::High,
            "email" => TaskPriority::Normal,
            "cli" => TaskPriority::Critical,
            _ => TaskPriority::Normal,
        }
    }
}

/// 处理器统计信息
#[derive(Debug, Clone)]
pub struct ProcessorStats {
    pub worker_pool: WorkerPoolStats,
    pub backpressure: Option<BackpressureStats>,
    pub circuit_breaker: Option<crate::concurrency::CircuitBreakerStats>,
    pub deduplicator: Option<crate::concurrency::DedupStats>,
}

/// 并发消息处理器 - 高级封装
///
/// 使用示例:
/// ```rust
/// let config = ConcurrentProcessorConfig::default();
/// let processor = ConcurrentMessageProcessor::new(config);
/// 
/// // 启动处理循环
/// processor.start(providers, memory, system_prompt).await;
/// ```
pub struct ConcurrentMessageProcessor {
    config: MessageProcessorConfig,
    breaker_group: CircuitBreakerGroup,
}

impl ConcurrentMessageProcessor {
    /// 创建新的并发消息处理器
    pub fn new(config: MessageProcessorConfig) -> Self {
        Self {
            breaker_group: CircuitBreakerGroup::with_config(config.circuit_config.clone()),
            config,
        }
    }

    /// 启动处理循环
    pub async fn start<P>(
        &self,
        channels: Vec<Arc<dyn Channel>>,
        provider: Arc<P>,
        system_prompt: String,
    ) -> anyhow::Result<()>
    where
        P: Provider + 'static,
    {
        let (processor, mut response_rx) = MessageProcessor::new(self.config.clone());
        
        info!(
            "Starting concurrent message processor with {} channels",
            channels.len()
        );

        // 启动响应处理任务
        let response_handler = tokio::spawn(async move {
            while let Some((channel_name, message, result)) = response_rx.recv().await {
                match result {
                    MessageResult::Success(response) => {
                        // 发送响应回通道
                        debug!("Sending response to {}: {}", channel_name, response);
                    }
                    MessageResult::Failed(err) => {
                        error!("Failed to process message: {}", err);
                    }
                    MessageResult::Timeout => {
                        warn!("Message processing timed out");
                    }
                    MessageResult::Duplicate => {
                        trace!("Duplicate message ignored");
                    }
                    MessageResult::Rejected(reason) => {
                        warn!("Message rejected: {}", reason);
                    }
                }
            }
        });

        // 启动消息接收
        let (msg_tx, mut msg_rx) = mpsc::channel::<ChannelMessage>(1000);
        
        for channel in &channels {
            let ch = Arc::clone(channel);
            let tx = msg_tx.clone();
            
            tokio::spawn(async move {
                if let Err(e) = ch.listen(tx).await {
                    error!("Channel {} error: {}", ch.name(), e);
                }
            });
        }
        drop(msg_tx);

        // 处理消息
        while let Some(message) = msg_rx.recv().await {
            let channel = channels
                .iter()
                .find(|c| c.name() == message.channel)
                .cloned();

            if let Some(channel) = channel {
                let provider = Arc::clone(&provider);
                let prompt = system_prompt.clone();

                let result = processor
                    .process_message(channel, message.clone(), move |msg| {
                        let provider = Arc::clone(&provider);
                        let prompt = prompt.clone();
                        async move {
                            provider
                                .chat_with_system(Some(&prompt), &msg.content, "default", 0.7)
                                .await
                        }
                    })
                    .await;

                trace!("Message {} submission result: {:?}", message.id, result);
            }
        }

        response_handler.abort();
        processor.shutdown().await;

        Ok(())
    }

    /// 获取所有熔断器状态
    pub fn circuit_breaker_stats(&self) -> HashMap<String, crate::concurrency::CircuitBreakerStats> {
        self.breaker_group.all_stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::traits::ChannelMessage;

    fn create_test_message(id: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.to_string(),
            sender: "test_user".to_string(),
            content: "Hello".to_string(),
            channel: "test".to_string(),
            timestamp: 0,
        }
    }

    #[test]
    fn test_message_processor_config_default() {
        let config = MessageProcessorConfig::default();
        assert_eq!(config.worker_pool_size, 4);
        assert_eq!(config.max_concurrent_requests, 10);
        assert!(config.enable_dedup);
        assert!(config.enable_circuit_breaker);
        assert!(config.enable_backpressure);
    }

    #[test]
    fn test_dedup_key_creation() {
        let message = create_test_message("1");
        
        let key1 = DedupKey::combine(vec![
            message.channel.clone(),
            message.sender.clone(),
            message.content.clone(),
        ]);
        
        let key2 = DedupKey::combine(vec![
            message.channel.clone(),
            message.sender.clone(),
            message.content.clone(),
        ]);
        
        assert_eq!(key1.hash_value(), key2.hash_value());
    }

    #[tokio::test]
    async fn test_message_processor_creation() {
        let config = MessageProcessorConfig::default();
        let (processor, mut response_rx) = MessageProcessor::new(config);
        
        let stats = processor.stats();
        assert_eq!(stats.worker_pool.worker_count, 4);
        
        processor.shutdown().await;
    }
}
