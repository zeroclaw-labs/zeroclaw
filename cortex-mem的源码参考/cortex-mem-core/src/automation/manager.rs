use crate::{
    Result,
    automation::{AutoExtractor, AutoIndexer, LayerGenerator},
    events::{CortexEvent, SessionEvent},
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// 自动化配置
#[derive(Debug, Clone)]
pub struct AutomationConfig {
    /// 是否启用自动索引
    pub auto_index: bool,
    /// 是否启用自动提取
    pub auto_extract: bool,
    /// 消息添加时是否立即索引（实时）
    pub index_on_message: bool,
    /// 会话关闭时是否索引（批量）
    pub index_on_close: bool,
    /// 索引批处理延迟（秒）
    pub index_batch_delay: u64,
    /// 启动时自动生成缺失的 L0/L1 文件
    pub auto_generate_layers_on_startup: bool,
    /// 每N条消息触发一次L0/L1生成（0表示禁用）
    pub generate_layers_every_n_messages: usize,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            auto_index: true,
            auto_extract: true,
            index_on_message: false, // 默认不实时索引（性能考虑）
            index_on_close: true,    // 默认会话关闭时索引
            index_batch_delay: 2,
            auto_generate_layers_on_startup: false, // 默认关闭（避免启动时阻塞）
            generate_layers_every_n_messages: 0,    // 默认禁用（避免频繁LLM调用）
        }
    }
}

/// 自动化管理器 - 统一调度索引和提取
pub struct AutomationManager {
    indexer: Arc<AutoIndexer>,
    extractor: Option<Arc<AutoExtractor>>,
    layer_generator: Option<Arc<LayerGenerator>>, // 层级生成器
    config: AutomationConfig,
}

impl AutomationManager {
    /// 创建自动化管理器
    pub fn new(
        indexer: Arc<AutoIndexer>,
        extractor: Option<Arc<AutoExtractor>>,
        config: AutomationConfig,
    ) -> Self {
        Self {
            indexer,
            extractor,
            layer_generator: None, // 初始为 None，需要单独设置
            config,
        }
    }

    /// 设置层级生成器（可选）
    pub fn with_layer_generator(mut self, layer_generator: Arc<LayerGenerator>) -> Self {
        self.layer_generator = Some(layer_generator);
        self
    }

    /// 🎯 核心方法：启动自动化任务
    pub async fn start(self, mut event_rx: mpsc::UnboundedReceiver<CortexEvent>) -> Result<()> {
        info!("Starting AutomationManager with config: {:?}", self.config);

        // 启动时自动生成缺失的 L0/L1 文件
        if self.config.auto_generate_layers_on_startup {
            if let Some(ref generator) = self.layer_generator {
                info!("启动时检查并生成缺失的 L0/L1 文件...");
                let generator_clone = generator.clone();
                tokio::spawn(async move {
                    match generator_clone.ensure_all_layers().await {
                        Ok(stats) => {
                            info!(
                                "启动时层级生成完成: 总计 {}, 成功 {}, 失败 {}",
                                stats.total, stats.generated, stats.failed
                            );
                        }
                        Err(e) => {
                            warn!("启动时层级生成失败: {}", e);
                        }
                    }
                });
            } else {
                warn!("auto_generate_layers_on_startup 已启用但未设置 layer_generator");
            }
        }

        // 批处理缓冲区（收集需要索引的session_id）
        let mut pending_sessions: HashSet<String> = HashSet::new();
        let batch_delay = Duration::from_secs(self.config.index_batch_delay);
        let mut batch_timer: Option<tokio::time::Instant> = None;

        // 会话消息计数器（用于触发定期L0/L1生成）
        let mut session_message_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        loop {
            tokio::select! {
                // 事件处理
                Some(event) = event_rx.recv() => {
                    if let Err(e) = self.handle_event(
                        event,
                        &mut pending_sessions,
                        &mut batch_timer,
                        batch_delay,
                        &mut session_message_counts
                    ).await {
                        warn!("Failed to handle event: {}", e);
                    }
                }

                // 批处理定时器触发
                _ = async {
                    if let Some(deadline) = batch_timer {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if !pending_sessions.is_empty() {
                        if let Err(e) = self.flush_batch(&mut pending_sessions).await {
                            warn!("Failed to flush batch: {}", e);
                        }
                        batch_timer = None;
                    }
                }
            }
        }
    }

    /// 处理事件
    async fn handle_event(
        &self,
        event: CortexEvent,
        pending_sessions: &mut HashSet<String>,
        batch_timer: &mut Option<tokio::time::Instant>,
        batch_delay: Duration,
        session_message_counts: &mut std::collections::HashMap<String, usize>,
    ) -> Result<()> {
        match event {
            CortexEvent::Session(SessionEvent::MessageAdded { session_id, .. }) => {
                // 更新消息计数
                let count = session_message_counts
                    .entry(session_id.clone())
                    .or_insert(0);
                *count += 1;

                // 检查是否需要基于消息数量触发L0/L1生成
                if self.config.generate_layers_every_n_messages > 0
                    && *count % self.config.generate_layers_every_n_messages == 0
                {
                    if let Some(ref generator) = self.layer_generator {
                        info!(
                            "Message count threshold reached ({} messages), triggering L0/L1 generation for session: {}",
                            count, session_id
                        );

                        // 异步生成L0/L1（避免阻塞）
                        let generator_clone = generator.clone();
                        let indexer_clone = self.indexer.clone();
                        let session_id_clone = session_id.clone();
                        let auto_index = self.config.auto_index;

                        tokio::spawn(async move {
                            let timeline_uri =
                                format!("cortex://session/{}/timeline", session_id_clone);

                            // 生成L0/L1
                            match generator_clone.ensure_timeline_layers(&timeline_uri).await {
                                Ok(stats) => {
                                    info!(
                                        "✓ Periodic L0/L1 generation for {}: total={}, generated={}, failed={}",
                                        session_id_clone,
                                        stats.total,
                                        stats.generated,
                                        stats.failed
                                    );

                                    // 生成后索引（如果启用了auto_index）
                                    if auto_index && stats.generated > 0 {
                                        match indexer_clone.index_thread(&session_id_clone).await {
                                            Ok(index_stats) => {
                                                info!(
                                                    "✓ L0/L1 indexed for {}: {} indexed",
                                                    session_id_clone, index_stats.total_indexed
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "✗ Failed to index L0/L1 for {}: {}",
                                                    session_id_clone, e
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "✗ Periodic L0/L1 generation failed for {}: {}",
                                        session_id_clone, e
                                    );
                                }
                            }
                        });
                    }
                }

                if self.config.index_on_message {
                    // 实时索引模式：立即索引
                    info!("Real-time indexing session: {}", session_id);
                    self.index_session(&session_id).await?;
                } else {
                    // 批处理模式：加入待处理队列
                    pending_sessions.insert(session_id);

                    // 启动批处理定时器（如果未启动）
                    if batch_timer.is_none() {
                        *batch_timer = Some(tokio::time::Instant::now() + batch_delay);
                    }
                }
            }

            CortexEvent::Session(SessionEvent::Closed { session_id }) => {
                if self.config.index_on_close {
                    info!(
                        "Session closed, triggering async full processing: {}",
                        session_id
                    );

                    // 🔧 异步执行所有后处理任务，避免阻塞事件循环
                    let extractor = self.extractor.clone();
                    let generator = self.layer_generator.clone();
                    let indexer = self.indexer.clone();
                    let auto_extract = self.config.auto_extract;
                    let auto_index = self.config.auto_index;
                    let session_id_clone = session_id.clone();

                    tokio::spawn(async move {
                        let start = tokio::time::Instant::now();

                        // 1. 自动提取记忆（如果配置了且有extractor）
                        if auto_extract {
                            if let Some(ref extractor) = extractor {
                                match extractor.extract_session(&session_id_clone).await {
                                    Ok(stats) => {
                                        info!(
                                            "✓ Extraction completed for {}: {:?}",
                                            session_id_clone, stats
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            "✗ Extraction failed for {}: {}",
                                            session_id_clone, e
                                        );
                                    }
                                }
                            }
                        }

                        // 2. 生成 L0/L1 层级文件（如果配置了layer_generator）
                        if let Some(ref generator) = generator {
                            info!("Generating L0/L1 layers for session: {}", session_id_clone);
                            let timeline_uri =
                                format!("cortex://session/{}/timeline", session_id_clone);

                            match generator.ensure_timeline_layers(&timeline_uri).await {
                                Ok(stats) => {
                                    info!(
                                        "✓ L0/L1 generation completed for {}: total={}, generated={}, failed={}",
                                        session_id_clone,
                                        stats.total,
                                        stats.generated,
                                        stats.failed
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "✗ L0/L1 generation failed for {}: {}",
                                        session_id_clone, e
                                    );
                                }
                            }
                        }

                        // 3. 索引整个会话（包括新生成的L0/L1/L2）
                        if auto_index {
                            match indexer.index_thread(&session_id_clone).await {
                                Ok(stats) => {
                                    info!(
                                        "✓ Session {} indexed: {} indexed, {} skipped, {} errors",
                                        session_id_clone,
                                        stats.total_indexed,
                                        stats.total_skipped,
                                        stats.total_errors
                                    );
                                }
                                Err(e) => {
                                    warn!("✗ Failed to index session {}: {}", session_id_clone, e);
                                }
                            }
                        }

                        let duration = start.elapsed();
                        info!(
                            "🎉 Session {} post-processing completed in {:.2}s",
                            session_id_clone,
                            duration.as_secs_f64()
                        );
                    });

                    info!(
                        "Session {} close acknowledged, post-processing running in background",
                        session_id
                    );
                }
            }

            _ => { /* 其他事件暂时忽略 */ }
        }

        Ok(())
    }

    /// 批量处理待索引的会话
    async fn flush_batch(&self, pending_sessions: &mut HashSet<String>) -> Result<()> {
        info!("Flushing batch: {} sessions", pending_sessions.len());

        for session_id in pending_sessions.drain() {
            if let Err(e) = self.index_session(&session_id).await {
                warn!("Failed to index session {}: {}", session_id, e);
            }
        }

        Ok(())
    }

    /// 索引单个会话
    async fn index_session(&self, session_id: &str) -> Result<()> {
        match self.indexer.index_thread(session_id).await {
            Ok(stats) => {
                info!(
                    "Session {} indexed: {} indexed, {} skipped, {} errors",
                    session_id, stats.total_indexed, stats.total_skipped, stats.total_errors
                );
                Ok(())
            }
            Err(e) => {
                warn!("Failed to index session {}: {}", session_id, e);
                Err(e)
            }
        }
    }
}
