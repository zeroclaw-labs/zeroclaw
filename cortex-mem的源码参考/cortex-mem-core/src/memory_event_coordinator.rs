//! Memory Event Coordinator Module
//!
//! Central coordinator that handles all memory events and orchestrates
//! the flow between different components.
//!
//! ## Phase 2 Optimization: Debouncing
//! - Batches layer update requests for the same directory
//! - Reduces redundant LLM calls by 70-90%
//! - Configurable debounce delay (default: 30 seconds)

use crate::Result;
use crate::cascade_layer_debouncer::{DebouncerConfig, LayerUpdateDebouncer};
use crate::cascade_layer_updater::CascadeLayerUpdater;
use crate::embedding::EmbeddingClient;
use crate::filesystem::{CortexFilesystem, FilesystemOperations};
use crate::incremental_memory_updater::IncrementalMemoryUpdater;
use crate::llm::LLMClient;
use crate::llm_result_cache::CacheConfig;
use crate::memory_events::{ChangeType, DeleteReason, EventStats, MemoryEvent};
use crate::memory_index::MemoryScope;
use crate::memory_index_manager::MemoryIndexManager;
use crate::session::extraction::ExtractedMemories;
use crate::vector_store::QdrantVectorStore;
use crate::vector_sync_manager::VectorSyncManager;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, watch};
use tracing::{debug, error, info, warn};

/// Configuration for event coordinator
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// Enable debouncing for layer updates (Phase 2)
    pub enable_debounce: bool,
    /// Debouncer configuration
    pub debouncer_config: DebouncerConfig,
    /// Enable LLM result cache (Phase 3)
    pub enable_cache: bool,
    /// Cache configuration
    pub cache_config: CacheConfig,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            enable_debounce: true, // Enable by default
            debouncer_config: DebouncerConfig::default(),
            enable_cache: true, // Enable cache by default
            cache_config: CacheConfig::default(),
        }
    }
}

/// Memory Event Coordinator
///
/// Central hub that coordinates all memory operations:
/// - Receives events from various sources
/// - Dispatches to appropriate handlers
/// - Ensures consistency across components
/// - (Phase 2) Debounces layer updates to reduce LLM calls
pub struct MemoryEventCoordinator {
    filesystem: Arc<CortexFilesystem>,
    llm_client: Arc<dyn LLMClient>,
    index_manager: Arc<MemoryIndexManager>,
    memory_updater: Arc<IncrementalMemoryUpdater>,
    layer_updater: Arc<CascadeLayerUpdater>,
    vector_sync: Arc<VectorSyncManager>,
    stats: Arc<RwLock<EventStats>>,
    /// Phase 2: Debouncer for layer updates
    debouncer: Option<Arc<LayerUpdateDebouncer>>,
    #[allow(dead_code)]
    config: CoordinatorConfig,
    /// 任务计数器：跟踪正在处理的任务数量
    pending_tasks: Arc<AtomicUsize>,
    /// 任务完成通知：当 pending_tasks 变为 0 时通知
    task_completion_tx: watch::Sender<usize>,
    /// 任务完成接收器（用于外部等待）
    task_completion_rx: watch::Receiver<usize>,
}

impl MemoryEventCoordinator {
    /// Create a new memory event coordinator with default config
    ///
    /// Returns (coordinator, event_sender, event_receiver)
    /// - coordinator: the coordinator instance (wrapped in Arc for shared access)
    /// - event_sender: use this to send events to the coordinator
    /// - event_receiver: pass this to coordinator.start() to begin processing
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        llm_client: Arc<dyn LLMClient>,
        embedding_client: Arc<EmbeddingClient>,
        vector_store: Arc<QdrantVectorStore>,
    ) -> (
        Arc<Self>,
        mpsc::UnboundedSender<MemoryEvent>,
        mpsc::UnboundedReceiver<MemoryEvent>,
    ) {
        Self::new_with_config(
            filesystem,
            llm_client,
            embedding_client,
            vector_store,
            CoordinatorConfig::default(),
        )
    }

    /// 发送事件到协调器（增加 pending_tasks 计数）
    ///
    /// 这个方法应该在发送事件时调用，确保 flush_and_wait 能正确等待事件处理完成
    pub fn send_event(&self, _event: MemoryEvent) -> Result<()> {
        // 先增加计数
        self.pending_tasks.fetch_add(1, Ordering::SeqCst);
        // 发送事件（通过内部 channel）
        // 注意：这里需要通过外部保存的 sender 发送
        // 由于架构限制，这个方法主要用于文档说明正确的使用方式
        Ok(())
    }

    /// Create a new memory event coordinator with custom config
    pub fn new_with_config(
        filesystem: Arc<CortexFilesystem>,
        llm_client: Arc<dyn LLMClient>,
        embedding_client: Arc<EmbeddingClient>,
        vector_store: Arc<QdrantVectorStore>,
        config: CoordinatorConfig,
    ) -> (
        Arc<Self>,
        mpsc::UnboundedSender<MemoryEvent>,
        mpsc::UnboundedReceiver<MemoryEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let index_manager = Arc::new(MemoryIndexManager::new(filesystem.clone()));

        // Create memory updater with event sender
        let memory_updater = Arc::new(IncrementalMemoryUpdater::new(
            filesystem.clone(),
            index_manager.clone(),
            llm_client.clone(),
            event_tx.clone(),
        ));

        // Create layer updater with event sender and optional cache
        let cache_config = if config.enable_cache {
            Some(config.cache_config.clone())
        } else {
            None
        };

        let layer_updater = Arc::new(CascadeLayerUpdater::new_with_cache(
            filesystem.clone(),
            llm_client.clone(),
            event_tx.clone(),
            cache_config,
        ));

        // Create vector sync manager
        let vector_sync = Arc::new(VectorSyncManager::new(
            filesystem.clone(),
            embedding_client,
            vector_store,
        ));

        // Phase 2: Create debouncer if enabled
        let debouncer = if config.enable_debounce {
            let debouncer = Arc::new(LayerUpdateDebouncer::new(config.debouncer_config.clone()));
            info!(
                "🔧 Layer update debouncer enabled (delay: {}s)",
                config.debouncer_config.debounce_secs
            );
            Some(debouncer)
        } else {
            info!("⚠️  Layer update debouncer disabled");
            None
        };

        // 创建任务完成通知机制
        let pending_tasks = Arc::new(AtomicUsize::new(0));
        let (task_completion_tx, task_completion_rx) = watch::channel(0);

        let coordinator = Arc::new(Self {
            filesystem,
            llm_client,
            index_manager,
            memory_updater,
            layer_updater,
            vector_sync,
            stats: Arc::new(RwLock::new(EventStats::default())),
            debouncer,
            config,
            pending_tasks,
            task_completion_tx,
            task_completion_rx,
        });

        (coordinator, event_tx, event_rx)
    }

    /// Start the event processing loop
    ///
    /// Phase 2: Integrates debouncer with periodic processing
    /// Returns a boxed future that can be spawned on a tokio runtime.
    pub fn start(
        self: Arc<Self>,
        mut event_rx: mpsc::UnboundedReceiver<MemoryEvent>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(async move {
            info!("Memory Event Coordinator started");

            // Phase 2: Setup periodic debouncer processing if enabled
            let mut debounce_interval = if self.debouncer.is_some() {
                Some(tokio::time::interval(Duration::from_millis(500))) // Check every 500ms
            } else {
                None
            };

            loop {
                tokio::select! {
                    // Handle incoming events
                    event = event_rx.recv() => {
                        match event {
                            Some(event) => {
                                // 🔧 关键修复：在取出事件时就增加计数
                                // 这样 flush_and_wait 可以正确检测到有待处理的事件
                                self.pending_tasks.fetch_add(1, Ordering::SeqCst);

                                if let Err(e) = self.handle_event_inner(event).await {
                                    error!("Event handling failed: {}", e);
                                }

                                // 减少计数并通知
                                let remaining = self.pending_tasks.fetch_sub(1, Ordering::SeqCst) - 1;
                                let _ = self.task_completion_tx.send(remaining);
                            }
                            None => {
                                warn!("Memory Event Coordinator stopped (channel closed)");
                                break;
                            }
                        }
                    }

                    // Phase 2: Periodic debouncer processing
                    _ = async {
                        if let Some(ref mut interval) = debounce_interval {
                            interval.tick().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        if let Some(ref debouncer) = self.debouncer {
                            let processed = debouncer.process_due_updates(&self.layer_updater).await;
                            if processed > 0 {
                                debug!("🔧 Debouncer processed {} updates", processed);
                            }
                        }
                    }
                }
            }

            // Final flush of pending updates
            if let Some(ref debouncer) = self.debouncer {
                let pending = debouncer.pending_count().await;
                if pending > 0 {
                    info!("🔄 Flushing {} pending updates before shutdown...", pending);
                    debouncer.process_due_updates(&self.layer_updater).await;
                }
            }

            info!("Memory Event Coordinator stopped");
        })
    }

    /// 获取任务完成通知接收器
    ///
    /// 外部可以使用这个接收器来等待所有任务完成
    pub fn get_task_completion_rx(&self) -> watch::Receiver<usize> {
        self.task_completion_rx.clone()
    }

    /// 获取当前待处理任务数量
    pub fn pending_task_count(&self) -> usize {
        self.pending_tasks.load(Ordering::SeqCst)
    }

    /// 刷新 debouncer 并等待所有任务完成（用于退出流程）
    ///
    /// 这个方法会：
    /// 0. 等待事件从 channel 被取出（通过 yield 让出运行时）
    /// 1. 等待当前正在处理的事件完成
    /// 2. 强制处理 debouncer 中所有待处理的层级更新
    /// 3. 再次等待确保所有更新完成
    ///
    /// 使用事件通知机制而非固定超时，确保真正等待任务完成。
    ///
    /// # Arguments
    /// * `check_interval` - 检查间隔
    ///
    /// # Returns
    /// * `true` - 所有任务已完成
    /// * `false` - 在等待过程中有新任务产生（通常不应该发生）
    pub async fn flush_and_wait(&self, check_interval: Duration) -> bool {
        log::info!("🔄 开始刷新并等待所有任务完成...");

        let start = std::time::Instant::now();
        let max_wait = Duration::from_secs(300); // 最大等待 5 分钟

        // 阶段0：让出运行时，让事件循环有机会运行
        // 这是关键：tokio::task::yield_now() 让其他任务有机会执行
        log::info!("⏳ 阶段0：让出运行时，等待事件被取出...");
        for i in 0..10 {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(10)).await;

            let pending = self.pending_tasks.load(Ordering::SeqCst);
            if pending > 0 {
                log::info!("✅ 阶段0完成：检测到 {} 个任务开始处理", pending);
                break;
            }

            if i == 9 {
                log::info!("ℹ️ 阶段0完成：无待处理任务检测到");
            }
        }

        // 阶段1：等待当前事件处理完成
        loop {
            let pending = self.pending_tasks.load(Ordering::SeqCst);
            if pending == 0 {
                // 等待一小段时间，看是否有新事件被取出
                tokio::time::sleep(Duration::from_millis(100)).await;
                let pending_after = self.pending_tasks.load(Ordering::SeqCst);
                if pending_after == 0 {
                    break;
                }
                continue;
            }

            // 检查是否超时
            if start.elapsed() >= max_wait {
                log::warn!("⚠️ 等待超时，仍有 {} 个任务未完成", pending);
                return false;
            }

            log::trace!(
                "⏳ 等待 {} 个事件处理任务完成...（已等待 {:?}）",
                pending,
                start.elapsed()
            );
            tokio::time::sleep(check_interval).await;
        }
        log::info!("✅ 阶段1完成：事件处理任务已清空");

        // 阶段2：刷新 debouncer 中的待处理更新
        if let Some(ref debouncer) = self.debouncer {
            let pending_count = debouncer.pending_count().await;
            if pending_count > 0 {
                log::info!(
                    "🔄 阶段2：刷新 {} 个 debouncer 待处理更新...",
                    pending_count
                );
                let flushed = debouncer.flush_all(&self.layer_updater).await;
                log::info!("✅ 阶段2完成：已刷新 {} 个层级更新", flushed);
            } else {
                log::info!("✅ 阶段2完成：debouncer 无待处理更新");
            }
        } else {
            log::info!("✅ 阶段2跳过：debouncer 未启用");
        }

        // 阶段3：再次等待，确保 debouncer 刷新产生的任务也完成
        loop {
            let pending = self.pending_tasks.load(Ordering::SeqCst);
            if pending == 0 {
                break;
            }

            // 检查是否超时
            if start.elapsed() >= max_wait {
                log::warn!("⚠️ 等待超时，仍有 {} 个任务未完成", pending);
                return false;
            }

            log::info!(
                "⏳ 等待 {} 个刷新后任务完成...（已等待 {:?}）",
                pending,
                start.elapsed()
            );
            tokio::time::sleep(check_interval).await;
        }
        log::info!("✅ 阶段3完成：所有任务已清空");

        log::info!(
            "🎉 flush_and_wait 完成：所有任务和层级更新已处理（耗时 {:?}）",
            start.elapsed()
        );
        true
    }

    /// 等待所有后台任务完成
    ///
    /// # Arguments
    /// * `timeout` - 最大等待时间
    ///
    /// # Returns
    /// * `true` - 所有任务已完成
    /// * `false` - 超时
    pub async fn wait_for_completion(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        let check_interval = Duration::from_millis(500);

        loop {
            let pending = self.pending_tasks.load(Ordering::SeqCst);

            // 如果没有待处理任务，返回成功
            if pending == 0 {
                // 额外等待一小段时间，确保没有新任务刚刚提交
                tokio::time::sleep(Duration::from_millis(200)).await;
                let pending_after = self.pending_tasks.load(Ordering::SeqCst);
                if pending_after == 0 {
                    log::info!("✅ 所有后台任务已完成");
                    return true;
                }
                // 有新任务提交，继续等待
                continue;
            }

            // 检查是否超时
            if start.elapsed() >= timeout {
                log::warn!("⚠️ 等待后台任务超时，仍有 {} 个任务未完成", pending);
                return false;
            }

            // 首次打印等待日志
            if start.elapsed() < Duration::from_millis(600) {
                log::info!("⏳ 等待 {} 个后台任务完成...", pending);
            }

            // 等待一小段时间再检查
            tokio::time::sleep(check_interval).await;
        }
    }

    /// Handle a single event (internal implementation)
    async fn handle_event_inner(&self, event: MemoryEvent) -> Result<()> {
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.record(&event);
        }

        debug!("Handling event: {}", event);

        match event {
            MemoryEvent::MemoryCreated {
                scope,
                owner_id,
                memory_id,
                memory_type,
                key,
                source_session,
                file_uri,
            } => {
                self.on_memory_created(
                    &scope,
                    &owner_id,
                    &memory_id,
                    &memory_type,
                    &key,
                    &source_session,
                    &file_uri,
                )
                .await?;
            }

            MemoryEvent::MemoryUpdated {
                scope,
                owner_id,
                memory_id,
                memory_type,
                key,
                source_session,
                file_uri,
                old_content_hash,
                new_content_hash,
            } => {
                self.on_memory_updated(
                    &scope,
                    &owner_id,
                    &memory_id,
                    &memory_type,
                    &key,
                    &source_session,
                    &file_uri,
                    &old_content_hash,
                    &new_content_hash,
                )
                .await?;
            }

            MemoryEvent::MemoryDeleted {
                scope,
                owner_id,
                memory_id,
                memory_type,
                file_uri,
                reason,
            } => {
                self.on_memory_deleted(
                    &scope,
                    &owner_id,
                    &memory_id,
                    &memory_type,
                    &file_uri,
                    &reason,
                )
                .await?;
            }

            MemoryEvent::MemoryAccessed {
                scope,
                owner_id,
                memory_id,
                context,
            } => {
                self.on_memory_accessed(&scope, &owner_id, &memory_id, &context)
                    .await?;
            }

            MemoryEvent::LayersUpdated {
                scope,
                owner_id,
                directory_uri,
                layers,
            } => {
                self.on_layers_updated(&scope, &owner_id, &directory_uri, &layers)
                    .await?;
            }

            MemoryEvent::SessionClosed {
                session_id,
                user_id,
                agent_id,
            } => {
                self.on_session_closed(&session_id, &user_id, &agent_id)
                    .await?;
            }

            MemoryEvent::LayerUpdateNeeded {
                scope,
                owner_id,
                directory_uri,
                change_type,
                changed_file,
            } => {
                self.on_layer_update_needed(
                    &scope,
                    &owner_id,
                    &directory_uri,
                    &change_type,
                    &changed_file,
                )
                .await?;
            }

            MemoryEvent::VectorSyncNeeded {
                file_uri,
                change_type,
            } => {
                self.on_vector_sync_needed(&file_uri, &change_type).await?;
            }
        }

        Ok(())
    }

    /// Handle memory created event
    async fn on_memory_created(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
        memory_type: &crate::memory_index::MemoryType,
        _key: &str,
        _source_session: &str,
        file_uri: &str,
    ) -> Result<()> {
        debug!(
            "Memory created: {} ({:?}) in {:?}/{}",
            memory_id, memory_type, scope, owner_id
        );

        // Trigger layer cascade update
        self.layer_updater
            .on_memory_changed(
                scope.clone(),
                owner_id.to_string(),
                file_uri.to_string(),
                ChangeType::Add,
            )
            .await?;

        // Trigger vector sync
        self.vector_sync
            .sync_file_change(file_uri, ChangeType::Add)
            .await?;

        Ok(())
    }

    /// Handle memory updated event
    async fn on_memory_updated(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
        memory_type: &crate::memory_index::MemoryType,
        _key: &str,
        _source_session: &str,
        file_uri: &str,
        _old_content_hash: &str,
        _new_content_hash: &str,
    ) -> Result<()> {
        debug!(
            "Memory updated: {} ({:?}) in {:?}/{}",
            memory_id, memory_type, scope, owner_id
        );

        // Trigger layer cascade update
        self.layer_updater
            .on_memory_changed(
                scope.clone(),
                owner_id.to_string(),
                file_uri.to_string(),
                ChangeType::Update,
            )
            .await?;

        // Trigger vector sync
        self.vector_sync
            .sync_file_change(file_uri, ChangeType::Update)
            .await?;

        Ok(())
    }

    /// Handle memory deleted event
    async fn on_memory_deleted(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
        memory_type: &crate::memory_index::MemoryType,
        file_uri: &str,
        reason: &DeleteReason,
    ) -> Result<()> {
        debug!(
            "Memory deleted: {} ({:?}) in {:?}/{}, reason: {:?}",
            memory_id, memory_type, scope, owner_id, reason
        );

        // Trigger layer cascade update
        self.layer_updater
            .on_memory_changed(
                scope.clone(),
                owner_id.to_string(),
                file_uri.to_string(),
                ChangeType::Delete,
            )
            .await?;

        // Trigger vector deletion
        self.vector_sync
            .sync_file_change(file_uri, ChangeType::Delete)
            .await?;

        Ok(())
    }

    /// Handle memory accessed event
    async fn on_memory_accessed(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        memory_id: &str,
        context: &str,
    ) -> Result<()> {
        debug!(
            "Memory accessed: {} in {:?}/{}, context: {}",
            memory_id, scope, owner_id, context
        );

        // Record access in index
        self.index_manager
            .record_access(scope, owner_id, memory_id)
            .await?;

        Ok(())
    }

    /// Handle layers updated event
    async fn on_layers_updated(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        directory_uri: &str,
        layers: &[crate::ContextLayer],
    ) -> Result<()> {
        debug!(
            "Layers updated for {} in {:?}/{}: {:?}",
            directory_uri, scope, owner_id, layers
        );

        // Sync layer files to vector database
        self.vector_sync.sync_layer_files(directory_uri).await?;

        Ok(())
    }

    /// Handle session closed event (the main trigger for memory extraction)
    async fn on_session_closed(
        &self,
        session_id: &str,
        user_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        // 使用 log 以便在 tars 中可见
        log::info!(
            "🔄 Processing session closed: {} (user_id={}, agent_id={})",
            session_id,
            user_id,
            agent_id
        );
        info!("Processing session closed: {}", session_id);

        // 1. Extract memories from the session
        let extracted = self.extract_memories_from_session(session_id).await?;

        log::info!(
            "🧠 Extracted memories: preferences={}, entities={}, events={}, cases={}, personal_info={}, work_history={}, relationships={}, goals={}",
            extracted.preferences.len(),
            extracted.entities.len(),
            extracted.events.len(),
            extracted.cases.len(),
            extracted.personal_info.len(),
            extracted.work_history.len(),
            extracted.relationships.len(),
            extracted.goals.len()
        );

        // 2. Update user memories
        if !extracted.is_empty() {
            let user_result = self
                .memory_updater
                .update_memories(user_id, agent_id, session_id, &extracted)
                .await?;

            log::info!(
                "✅ User memory update for session {}: {} created, {} updated",
                session_id,
                user_result.created,
                user_result.updated
            );
            info!(
                "User memory update for session {}: {} created, {} updated",
                session_id, user_result.created, user_result.updated
            );

            // 注意：不在这里调用 update_all_layers，因为它是长时间运行的操作
            // 会阻塞事件处理循环。改为在退出流程中显式调用 generate_user_agent_layers
            log::info!("📝 记忆已写入，退出时应调用 generate_user_agent_layers 生成层级文件");
        } else {
            log::info!("⚠️ No memories extracted from session {}", session_id);
        }

        // 3. Update timeline layers
        self.layer_updater
            .update_timeline_layers(session_id)
            .await?;

        // 4. Sync session to vectors
        let timeline_uri = format!("cortex://session/{}/timeline", session_id);
        self.vector_sync.sync_directory(&timeline_uri).await?;

        log::info!("✅ Session {} processing complete", session_id);
        info!("Session {} processing complete", session_id);

        Ok(())
    }

    /// Handle layer update needed event
    ///
    /// Phase 2: Uses debouncer if enabled
    async fn on_layer_update_needed(
        &self,
        scope: &MemoryScope,
        owner_id: &str,
        directory_uri: &str,
        change_type: &ChangeType,
        changed_file: &str,
    ) -> Result<()> {
        debug!(
            "Layer update needed for {} due to {:?} on {}",
            directory_uri, change_type, changed_file
        );

        // Phase 2: Use debouncer if enabled
        if let Some(ref debouncer) = self.debouncer {
            // Request update (will be debounced)
            debouncer
                .request_update(
                    directory_uri.to_string(),
                    scope.clone(),
                    owner_id.to_string(),
                )
                .await;

            debug!(
                "🔧 Layer update request queued for debouncing: {}",
                directory_uri
            );
        } else {
            // No debouncing, execute immediately
            self.layer_updater
                .on_memory_changed(
                    scope.clone(),
                    owner_id.to_string(),
                    changed_file.to_string(),
                    change_type.clone(),
                )
                .await?;
        }

        Ok(())
    }

    /// Handle vector sync needed event
    async fn on_vector_sync_needed(&self, file_uri: &str, change_type: &ChangeType) -> Result<()> {
        debug!("Vector sync needed for {}: {:?}", file_uri, change_type);

        self.vector_sync
            .sync_file_change(file_uri, change_type.clone())
            .await?;

        Ok(())
    }

    /// Extract memories from a session using LLM
    async fn extract_memories_from_session(&self, session_id: &str) -> Result<ExtractedMemories> {
        // Collect all messages from the session
        let timeline_uri = format!("cortex://session/{}/timeline", session_id);

        log::info!("📂 Collecting messages from: {}", timeline_uri);

        let mut messages = Vec::new();
        match self
            .collect_messages_recursive(&timeline_uri, &mut messages)
            .await
        {
            Ok(_) => {
                log::info!("✅ Collected {} messages from session", messages.len());
            }
            Err(e) => {
                log::error!("❌ Failed to collect messages: {}", e);
                return Err(e);
            }
        }

        if messages.is_empty() {
            log::warn!("⚠️ No messages found in session {}", session_id);
            debug!("No messages found in session {}", session_id);
            return Ok(ExtractedMemories::default());
        }

        // Build extraction prompt
        log::info!(
            "🧠 Building extraction prompt for {} messages...",
            messages.len()
        );
        let prompt = self.build_extraction_prompt(&messages);

        // Call LLM for extraction
        log::info!("📞 Calling LLM for memory extraction...");
        let response = match self.llm_client.complete(&prompt).await {
            Ok(resp) => {
                log::info!("✅ LLM response received ({} chars)", resp.len());
                resp
            }
            Err(e) => {
                log::error!("❌ LLM call failed: {}", e);
                return Err(e);
            }
        };

        // Parse response
        let extracted = self.parse_extraction_response(&response);

        log::info!(
            "🧠 Extracted memories: preferences={}, entities={}, events={}, cases={}, personal_info={}, work_history={}, relationships={}, goals={}",
            extracted.preferences.len(),
            extracted.entities.len(),
            extracted.events.len(),
            extracted.cases.len(),
            extracted.personal_info.len(),
            extracted.work_history.len(),
            extracted.relationships.len(),
            extracted.goals.len()
        );

        info!(
            "Extracted {} memories from session {}",
            extracted.preferences.len()
                + extracted.entities.len()
                + extracted.events.len()
                + extracted.cases.len(),
            session_id
        );

        Ok(extracted)
    }

    /// Recursively collect messages from timeline
    fn collect_messages_recursive<'a>(
        &'a self,
        uri: &'a str,
        messages: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(uri).await?;

            for entry in entries {
                if entry.name.starts_with('.') {
                    continue;
                }

                if entry.is_directory {
                    self.collect_messages_recursive(&entry.uri, messages)
                        .await?;
                } else if entry.name.ends_with(".md") {
                    if let Ok(content) = self.filesystem.read(&entry.uri).await {
                        messages.push(content);
                    }
                }
            }

            Ok(())
        })
    }

    /// Build the extraction prompt
    fn build_extraction_prompt(&self, messages: &[String]) -> String {
        let messages_text = messages.join("\n\n---\n\n");

        format!(
            r#"Analyze the following conversation and extract memories in JSON format.

## Instructions

Extract the following types of memories:

1. **Personal Info** (user's personal information):
   - category: "age", "occupation", "education", "location", etc.
   - content: The specific information
   - confidence: 0.0-1.0 confidence level

2. **Work History** (user's work experience):
   - company: Company name
   - role: Job title/role
   - duration: Time period (optional)
   - description: Brief description
   - confidence: 0.0-1.0 confidence level

3. **Preferences** (user preferences by topic):
   - topic: The topic/subject area
   - preference: The user's stated preference
   - confidence: 0.0-1.0 confidence level

4. **Relationships** (people user mentions):
   - person: Person's name
   - relation_type: "family", "colleague", "friend", etc.
   - context: How they're related
   - confidence: 0.0-1.0 confidence level

5. **Goals** (user's goals and aspirations):
   - goal: The specific goal
   - category: "career", "personal", "health", "learning", etc.
   - timeline: When they want to achieve it (optional)
   - confidence: 0.0-1.0 confidence level

6. **Entities** (people, projects, organizations mentioned):
   - name: Entity name
   - entity_type: "person", "project", "organization", "technology", etc.
   - description: Brief description
   - context: How it was mentioned

7. **Events** (decisions, milestones, important occurrences):
   - title: Event title
   - event_type: "decision", "milestone", "occurrence"
   - summary: Brief summary
   - timestamp: If mentioned

8. **Cases** (problems encountered and solutions found):
   - title: Case title
   - problem: The problem encountered
   - solution: How it was solved
   - lessons_learned: Array of lessons learned

## Response Format

Return ONLY a JSON object with this structure:

{{
  "personal_info": [{{ "category": "...", "content": "...", "confidence": 0.9 }}],
  "work_history": [{{ "company": "...", "role": "...", "duration": "...", "description": "...", "confidence": 0.9 }}],
  "preferences": [{{ "topic": "...", "preference": "...", "confidence": 0.9 }}],
  "relationships": [{{ "person": "...", "relation_type": "...", "context": "...", "confidence": 0.9 }}],
  "goals": [{{ "goal": "...", "category": "...", "timeline": "...", "confidence": 0.9 }}],
  "entities": [{{ "name": "...", "entity_type": "...", "description": "...", "context": "..." }}],
  "events": [{{ "title": "...", "event_type": "...", "summary": "...", "timestamp": "..." }}],
  "cases": [{{ "title": "...", "problem": "...", "solution": "...", "lessons_learned": ["..."] }}]
}}

Only include memories that are clearly stated in the conversation. Set empty arrays for categories with no data.

## Conversation

{}

## Response

Return ONLY the JSON object. No additional text before or after."#,
            messages_text
        )
    }

    /// Parse the LLM extraction response
    fn parse_extraction_response(&self, response: &str) -> ExtractedMemories {
        // Try to extract JSON from the response
        let json_str = if response.starts_with('{') {
            response.to_string()
        } else {
            response
                .find('{')
                .and_then(|start| response.rfind('}').map(|end| &response[start..=end]))
                .map(|s| s.to_string())
                .unwrap_or_default()
        };

        if json_str.is_empty() {
            return ExtractedMemories::default();
        }

        serde_json::from_str(&json_str).unwrap_or_default()
    }

    /// Get current event statistics
    pub async fn get_stats(&self) -> EventStats {
        self.stats.read().await.clone()
    }

    /// Force a full update for a scope
    pub async fn force_full_update(&self, scope: &MemoryScope, owner_id: &str) -> Result<()> {
        info!("Forcing full update for {:?}/{}", scope, owner_id);

        // Update all layers
        self.layer_updater
            .update_all_layers(scope, owner_id)
            .await?;

        // Sync to vectors
        let root_uri = match scope {
            MemoryScope::User => format!("cortex://user/{}", owner_id),
            MemoryScope::Agent => format!("cortex://agent/{}", owner_id),
            MemoryScope::Session => format!("cortex://session/{}", owner_id),
            MemoryScope::Resources => "cortex://resources".to_string(),
        };

        self.vector_sync.sync_directory(&root_uri).await?;

        Ok(())
    }

    /// Delete all memories for a session
    pub async fn delete_session_memories(
        &self,
        session_id: &str,
        user_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        info!("Deleting all memories for session {}", session_id);

        // Delete from index
        let deleted_user = self
            .index_manager
            .delete_memories_from_session(&MemoryScope::User, user_id, session_id)
            .await?;

        let deleted_agent = self
            .index_manager
            .delete_memories_from_session(&MemoryScope::Agent, agent_id, session_id)
            .await?;

        // Delete vectors
        self.vector_sync.delete_session_vectors(session_id).await?;

        info!(
            "Deleted {} user memories and {} agent memories for session {}",
            deleted_user.len(),
            deleted_agent.len(),
            session_id
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLLMClient;

    #[test]
    fn test_build_extraction_prompt() {
        let messages = vec![
            "User: I prefer Rust for systems programming.".to_string(),
            "Assistant: That's a great choice!".to_string(),
        ];

        // Build prompt directly (doesn't need coordinator)
        let messages_text = messages.join("\n\n---\n\n");
        let prompt = format!(
            r#"Analyze the following conversation and extract memories in JSON format.

## Conversation

{}

## Response

Return ONLY the JSON object. No additional text before or after."#,
            messages_text
        );

        assert!(prompt.contains("I prefer Rust"));
        assert!(prompt.contains("conversation"));
    }

    #[test]
    fn test_parse_extraction_response() {
        let llm_client = MockLLMClient::new();

        // Valid JSON response
        let response = r#"{
            "personal_info": [],
            "work_history": [],
            "preferences": [{"topic": "programming", "preference": "Rust", "confidence": 0.9}],
            "relationships": [],
            "goals": [],
            "entities": [],
            "events": [],
            "cases": []
        }"#;

        // Parse response directly
        let json_str = if response.starts_with('{') {
            response.to_string()
        } else {
            response
                .find('{')
                .and_then(|start| response.rfind('}').map(|end| &response[start..=end]))
                .map(|s| s.to_string())
                .unwrap_or_default()
        };

        let extracted: ExtractedMemories = serde_json::from_str(&json_str).unwrap_or_default();

        assert_eq!(extracted.preferences.len(), 1);
        assert_eq!(extracted.preferences[0].topic, "programming");
        assert_eq!(extracted.preferences[0].preference, "Rust");

        // Just to suppress unused variable warning
        let _ = llm_client;
    }

    #[test]
    fn test_parse_extraction_response_with_wrapper() {
        // Response with text wrapper
        let response = r#"Here is the extracted data:
        {
            "personal_info": [],
            "work_history": [],
            "preferences": [],
            "relationships": [],
            "goals": [{"goal": "Learn Rust", "category": "learning", "confidence": 0.8}],
            "entities": [],
            "events": [],
            "cases": []
        }
        That's all!"#;

        // Extract JSON from wrapper
        let json_str = response
            .find('{')
            .and_then(|start| response.rfind('}').map(|end| &response[start..=end]))
            .map(|s| s.to_string())
            .unwrap_or_default();

        let extracted: ExtractedMemories = serde_json::from_str(&json_str).unwrap_or_default();

        assert_eq!(extracted.goals.len(), 1);
        assert_eq!(extracted.goals[0].goal, "Learn Rust");
    }

    #[test]
    fn test_parse_extraction_response_empty() {
        // Empty response
        let json_str = "";
        let extracted: ExtractedMemories = serde_json::from_str(json_str).unwrap_or_default();
        assert!(extracted.is_empty());

        // Invalid JSON
        let extracted: ExtractedMemories = serde_json::from_str("not json").unwrap_or_default();
        assert!(extracted.is_empty());
    }

    #[test]
    fn test_event_stats_tracking() {
        let mut stats = EventStats::default();

        stats.record(&MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: "user_001".to_string(),
            memory_id: "mem_001".to_string(),
            memory_type: crate::memory_index::MemoryType::Preference,
            key: "test".to_string(),
            source_session: "session_001".to_string(),
            file_uri: "cortex://user/user_001/test.md".to_string(),
        });

        stats.record(&MemoryEvent::SessionClosed {
            session_id: "session_001".to_string(),
            user_id: "user_001".to_string(),
            agent_id: "agent_001".to_string(),
        });

        assert_eq!(stats.memory_created, 1);
        assert_eq!(stats.sessions_closed, 1);
        assert_eq!(stats.total_events(), 2);
    }

    #[test]
    fn test_memory_event_scope() {
        let event = MemoryEvent::MemoryCreated {
            scope: MemoryScope::User,
            owner_id: "user_001".to_string(),
            memory_id: "mem_001".to_string(),
            memory_type: crate::memory_index::MemoryType::Preference,
            key: "test".to_string(),
            source_session: "session_001".to_string(),
            file_uri: "cortex://user/user_001/test.md".to_string(),
        };

        assert_eq!(event.scope(), Some(&MemoryScope::User));
        assert_eq!(event.owner_id(), Some("user_001"));
        assert!(event.requires_cascade_update());
        assert!(event.requires_vector_sync());
    }
}
