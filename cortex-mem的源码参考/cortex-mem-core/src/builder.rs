/// 统一初始化API模块
/// 提供Builder模式的一站式初始化接口
use crate::{
    Result,
    embedding::{EmbeddingClient, EmbeddingConfig},
    events::EventBus,
    filesystem::CortexFilesystem,
    llm::LLMClient,
    memory_event_coordinator::{CoordinatorConfig, MemoryEventCoordinator},
    session::{SessionConfig, SessionManager},
    vector_store::{QdrantVectorStore, VectorStore},
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// 🎯 一站式初始化cortex-mem，包含自动化功能
pub struct CortexMemBuilder {
    data_dir: PathBuf,
    embedding_config: Option<EmbeddingConfig>,
    qdrant_config: Option<crate::config::QdrantConfig>,
    llm_client: Option<Arc<dyn LLMClient>>,
    session_config: SessionConfig,
    /// v2.5: 事件协调器配置
    coordinator_config: Option<CoordinatorConfig>,
}

impl CortexMemBuilder {
    /// 创建新的构建器
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            embedding_config: None,
            qdrant_config: None,
            llm_client: None,
            session_config: SessionConfig::default(),
            coordinator_config: None,
        }
    }

    /// 配置Embedding服务
    pub fn with_embedding(mut self, config: EmbeddingConfig) -> Self {
        self.embedding_config = Some(config);
        self
    }

    /// 配置Qdrant向量数据库
    pub fn with_qdrant(mut self, config: crate::config::QdrantConfig) -> Self {
        self.qdrant_config = Some(config);
        self
    }

    /// 配置LLM客户端
    pub fn with_llm(mut self, llm_client: Arc<dyn LLMClient>) -> Self {
        self.llm_client = Some(llm_client);
        self
    }

    /// 配置会话管理
    pub fn with_session_config(mut self, config: SessionConfig) -> Self {
        self.session_config = config;
        self
    }

    /// v2.5: 配置事件协调器
    pub fn with_coordinator_config(mut self, config: CoordinatorConfig) -> Self {
        self.coordinator_config = Some(config);
        self
    }

    /// 🎯 构建完整的cortex-mem实例
    pub async fn build(self) -> Result<CortexMem> {
        info!("Building Cortex Memory with v2.5 incremental update support");

        // 1. 初始化文件系统
        let filesystem = Arc::new(CortexFilesystem::new(
            self.data_dir.to_string_lossy().as_ref(),
        ));
        filesystem.initialize().await?;
        info!("Filesystem initialized at: {:?}", self.data_dir);

        // 2. 初始化Embedding客户端（可选）
        let embedding = if let Some(cfg) = self.embedding_config {
            match EmbeddingClient::new(cfg) {
                Ok(client) => Some(Arc::new(client)),
                Err(e) => {
                    warn!("Failed to create embedding client: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // 3. 初始化Qdrant向量存储（可选）
        let vector_store: Option<Arc<dyn VectorStore>> = if let Some(ref cfg) = self.qdrant_config {
            match QdrantVectorStore::new(cfg).await {
                Ok(store) => {
                    info!("Qdrant vector store connected: {}", cfg.url);
                    Some(Arc::new(store))
                }
                Err(e) => {
                    warn!("Failed to connect to Qdrant, vector search disabled: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // 4. 创建事件总线（用于向后兼容）
        let (event_bus, _old_event_rx) = EventBus::new();
        let event_bus = Arc::new(event_bus);

        // 5. v2.5: 创建 MemoryEventCoordinator（如果配置了所有必需组件）
        let (coordinator_handle, memory_event_tx) = 
            if let (Some(llm), Some(emb), Some(_vs)) = 
                (&self.llm_client, &embedding, &vector_store) 
            {
                // 将 VectorStore trait object 转换为 QdrantVectorStore
                // 由于我们需要具体类型，这里重新从配置创建
                let qdrant_store = if let Some(ref cfg) = self.qdrant_config {
                    match QdrantVectorStore::new(cfg).await {
                        Ok(store) => Arc::new(store),
                        Err(e) => {
                            warn!("Failed to create QdrantVectorStore for coordinator: {}", e);
                            let fs = filesystem.clone();
                            return Ok(CortexMem {
                                filesystem: fs.clone(),
                                session_manager: Arc::new(RwLock::new(
                                    SessionManager::with_event_bus(
                                        fs,
                                        self.session_config,
                                        event_bus.as_ref().clone(),
                                    )
                                )),
                                embedding,
                                vector_store,
                                llm_client: self.llm_client,
                                event_bus,
                                coordinator_handle: None,
                            });
                        }
                    }
                } else {
                    warn!("No Qdrant config available for coordinator");
                    let fs = filesystem.clone();
                    return Ok(CortexMem {
                        filesystem: fs.clone(),
                        session_manager: Arc::new(RwLock::new(
                            SessionManager::with_event_bus(
                                fs,
                                self.session_config,
                                event_bus.as_ref().clone(),
                            )
                        )),
                        embedding,
                        vector_store,
                        llm_client: self.llm_client,
                        event_bus,
                        coordinator_handle: None,
                    });
                };

                let config = self.coordinator_config.unwrap_or_default();
                let (coordinator, tx, rx) = MemoryEventCoordinator::new_with_config(
                    filesystem.clone(),
                    llm.clone(),
                    emb.clone(),
                    qdrant_store,
                    config,
                );

                // 启动事件协调器
                let handle = tokio::spawn(coordinator.start(rx));
                info!("✅ MemoryEventCoordinator started for v2.5 incremental updates");

                (Some(handle), Some(tx))
            } else {
                warn!("MemoryEventCoordinator disabled: missing LLM, embedding, or vector store");
                (None, None)
            };

        // 6. 创建SessionManager（带 v2.5 memory_event_tx）
        let session_manager = if let Some(tx) = memory_event_tx {
            // v2.5: 使用 MemoryEventCoordinator 的事件通道
            if let Some(ref llm) = self.llm_client {
                SessionManager::with_llm_and_events(
                    filesystem.clone(),
                    self.session_config,
                    llm.clone(),
                    event_bus.as_ref().clone(),
                )
                .with_memory_event_tx(tx)
            } else {
                SessionManager::with_event_bus(
                    filesystem.clone(),
                    self.session_config,
                    event_bus.as_ref().clone(),
                )
                .with_memory_event_tx(tx)
            }
        } else {
            // 回退到旧的事件总线机制
            if let Some(ref llm) = self.llm_client {
                SessionManager::with_llm_and_events(
                    filesystem.clone(),
                    self.session_config,
                    llm.clone(),
                    event_bus.as_ref().clone(),
                )
            } else {
                SessionManager::with_event_bus(
                    filesystem.clone(),
                    self.session_config,
                    event_bus.as_ref().clone(),
                )
            }
        };

        info!("✅ CortexMem initialized successfully");

        Ok(CortexMem {
            filesystem,
            session_manager: Arc::new(RwLock::new(session_manager)),
            embedding,
            vector_store,
            llm_client: self.llm_client,
            event_bus,
            coordinator_handle,
        })
    }
}

/// CortexMem实例 - 统一封装所有功能
pub struct CortexMem {
    pub filesystem: Arc<CortexFilesystem>,
    pub session_manager: Arc<RwLock<SessionManager>>,
    pub embedding: Option<Arc<EmbeddingClient>>,
    pub vector_store: Option<Arc<dyn VectorStore>>,
    pub llm_client: Option<Arc<dyn LLMClient>>,
    #[allow(dead_code)]
    event_bus: Arc<EventBus>,
    /// v2.5: MemoryEventCoordinator 的后台任务句柄
    coordinator_handle: Option<tokio::task::JoinHandle<()>>,
}

impl CortexMem {
    /// 获取SessionManager
    pub fn session_manager(&self) -> Arc<RwLock<SessionManager>> {
        self.session_manager.clone()
    }

    /// 获取文件系统
    pub fn filesystem(&self) -> Arc<CortexFilesystem> {
        self.filesystem.clone()
    }

    /// 获取Embedding客户端
    pub fn embedding(&self) -> Option<Arc<EmbeddingClient>> {
        self.embedding.clone()
    }

    /// 获取向量存储
    pub fn vector_store(&self) -> Option<Arc<dyn VectorStore>> {
        self.vector_store.clone()
    }

    /// 获取LLM客户端
    pub fn llm_client(&self) -> Option<Arc<dyn LLMClient>> {
        self.llm_client.clone()
    }

    /// 优雅关闭
    pub async fn shutdown(self) -> Result<()> {
        info!("Shutting down CortexMem...");

        // 停止 MemoryEventCoordinator
        if let Some(handle) = self.coordinator_handle {
            handle.abort();
            info!("MemoryEventCoordinator stopped");
        }

        Ok(())
    }
}