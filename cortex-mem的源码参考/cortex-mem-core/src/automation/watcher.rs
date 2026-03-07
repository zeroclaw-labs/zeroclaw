use crate::{
    automation::AutoIndexer,
    filesystem::{CortexFilesystem, FilesystemOperations},
    Result,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// 文件系统变化事件
#[derive(Debug, Clone)]
pub enum FsEvent {
    /// 新消息添加
    MessageAdded {
        thread_id: String,
        message_id: String,
    },
    /// 消息更新
    MessageUpdated {
        thread_id: String,
        message_id: String,
    },
    /// 线程删除
    ThreadDeleted { thread_id: String },
}

/// 文件监听器配置
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// 轮询间隔（秒）
    pub poll_interval_secs: u64,
    /// 是否自动索引
    pub auto_index: bool,
    /// 批处理延迟（秒）
    pub batch_delay_secs: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 5,
            auto_index: true,
            batch_delay_secs: 2,
        }
    }
}

/// 文件系统监听器
///
/// 监听cortex文件系统的变化，触发自动索引
pub struct FsWatcher {
    filesystem: Arc<CortexFilesystem>,
    indexer: Arc<AutoIndexer>,
    config: WatcherConfig,
    event_tx: mpsc::UnboundedSender<FsEvent>,
    event_rx: Option<mpsc::UnboundedReceiver<FsEvent>>,
}

impl FsWatcher {
    /// 创建新的监听器
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        indexer: Arc<AutoIndexer>,
        config: WatcherConfig,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        Self {
            filesystem,
            indexer,
            config,
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// 启动监听器
    pub async fn start(mut self) -> Result<()> {
        info!("Starting filesystem watcher with {:?}", self.config);

        let event_rx = self
            .event_rx
            .take()
            .ok_or_else(|| crate::Error::Other("Event receiver already taken".to_string()))?;

        // 启动事件处理任务
        let indexer = self.indexer.clone();
        let config = self.config.clone();
        tokio::spawn(async move {
            Self::process_events(event_rx, indexer, config).await;
        });

        // 启动轮询任务
        self.poll_filesystem().await
    }

    /// 轮询文件系统变化
    async fn poll_filesystem(&self) -> Result<()> {
        let mut last_thread_state = std::collections::HashMap::new();

        loop {
            tokio::time::sleep(Duration::from_secs(self.config.poll_interval_secs)).await;

            match self.scan_for_changes(&mut last_thread_state).await {
                Ok(events) => {
                    for event in events {
                        if let Err(e) = self.event_tx.send(event) {
                            warn!("Failed to send event: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Error scanning filesystem: {}", e);
                }
            }
        }
    }

    /// 扫描文件系统变化
    async fn scan_for_changes(
        &self,
        last_state: &mut std::collections::HashMap<String, Vec<String>>,
    ) -> Result<Vec<FsEvent>> {
        let threads_uri = "cortex://session";
        let entries = self.filesystem.list(threads_uri).await?;

        let mut events = Vec::new();

        for entry in entries {
            if !entry.is_directory || entry.name.starts_with('.') {
                continue;
            }

            let thread_id = entry.name.clone();
            let timeline_uri = format!("cortex://session/{}/timeline", thread_id);

            // 获取当前线程的所有消息
            match self.get_message_ids(&timeline_uri).await {
                Ok(current_messages) => {
                    let previous_messages = last_state.get(&thread_id);

                    if let Some(prev) = previous_messages {
                        // 检测新消息
                        for msg_id in &current_messages {
                            if !prev.contains(msg_id) {
                                debug!("New message detected: {} in thread {}", msg_id, thread_id);
                                events.push(FsEvent::MessageAdded {
                                    thread_id: thread_id.clone(),
                                    message_id: msg_id.clone(),
                                });
                            }
                        }
                    }

                    last_state.insert(thread_id, current_messages);
                }
                Err(e) => {
                    warn!("Failed to scan thread {}: {}", thread_id, e);
                }
            }
        }

        Ok(events)
    }

    /// 获取线程中的所有消息ID
    async fn get_message_ids(&self, timeline_uri: &str) -> Result<Vec<String>> {
        let mut message_ids = Vec::new();
        self.collect_message_ids_recursive(timeline_uri, &mut message_ids)
            .await?;
        Ok(message_ids)
    }

    /// 递归收集消息ID
    fn collect_message_ids_recursive<'a>(
        &'a self,
        uri: &'a str,
        message_ids: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.as_ref().list(uri).await?;

            for entry in entries {
                if entry.is_directory && !entry.name.starts_with('.') {
                    self.collect_message_ids_recursive(&entry.uri, message_ids)
                        .await?;
                } else if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                    // 从文件名提取消息ID
                    if let Some(msg_id) = entry.name.strip_suffix(".md") {
                        message_ids.push(msg_id.to_string());
                    }
                }
            }

            Ok(())
        })
    }

    /// 处理事件
    async fn process_events(
        mut event_rx: mpsc::UnboundedReceiver<FsEvent>,
        indexer: Arc<AutoIndexer>,
        config: WatcherConfig,
    ) {
        let mut pending_threads = std::collections::HashSet::new();

        loop {
            tokio::select! {
                Some(event) = event_rx.recv() => {
                    match event {
                        FsEvent::MessageAdded { thread_id, message_id } => {
                            info!("Processing new message: {} in thread {}", message_id, thread_id);
                            if config.auto_index {
                                pending_threads.insert(thread_id);
                            }
                        }
                        FsEvent::MessageUpdated { thread_id, message_id } => {
                            debug!("Message updated: {} in thread {}", message_id, thread_id);
                            if config.auto_index {
                                pending_threads.insert(thread_id);
                            }
                        }
                        FsEvent::ThreadDeleted { thread_id } => {
                            info!("Thread deleted: {}", thread_id);
                            pending_threads.remove(&thread_id);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(config.batch_delay_secs)) => {
                    // 批量处理待索引的线程
                    if !pending_threads.is_empty() {
                        let threads: Vec<_> = pending_threads.drain().collect();
                        for thread_id in threads {
                            match indexer.index_thread(&thread_id).await {
                                Ok(stats) => {
                                    info!("Auto-indexed thread {}: {} messages", thread_id, stats.total_indexed);
                                }
                                Err(e) => {
                                    warn!("Failed to auto-index thread {}: {}", thread_id, e);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
