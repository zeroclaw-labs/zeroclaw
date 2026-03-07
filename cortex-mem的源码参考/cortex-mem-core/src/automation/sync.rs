use crate::{
    embedding::EmbeddingClient,
    filesystem::{CortexFilesystem, FilesystemOperations},
    layers::manager::LayerManager,
    llm::LLMClient,
    types::{Memory, MemoryMetadata},
    vector_store::{QdrantVectorStore, uri_to_vector_id},
    ContextLayer,
    Result,
};
use std::sync::Arc;
use tracing::{debug, info, warn};

// Import VectorStore trait to use its methods
use crate::vector_store::VectorStore as _;

/// 自动同步管理器
///
/// 负责：
/// 1. 扫描文件系统中的所有Markdown文件
/// 2. 为未索引的文件生成embedding
/// 3. 批量同步到Qdrant向量数据库
/// 4. 支持增量更新
pub struct SyncManager {
    filesystem: Arc<CortexFilesystem>,
    embedding: Arc<EmbeddingClient>,
    vector_store: Arc<crate::vector_store::QdrantVectorStore>,
    llm_client: Arc<dyn LLMClient>,
    config: SyncConfig,
}

/// 自动同步管理器配置
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// 是否自动索引新消息
    pub auto_index: bool,
    /// 是否同步agents维度
    pub sync_agents: bool,
    /// 是否同步threads维度
    pub sync_threads: bool,
    /// 是否同步users维度
    pub sync_users: bool,
    /// 是否同步global维度
    pub sync_global: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_index: true,
            sync_agents: true,
            sync_threads: true,
            sync_users: true,
            sync_global: true,
        }
    }
}

/// 同步统计
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    pub total_files: usize,
    pub indexed_files: usize,
    pub skipped_files: usize,
    pub error_files: usize,
}

impl SyncManager {
    /// 创建新的自动同步管理器
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        embedding: Arc<EmbeddingClient>,
        vector_store: Arc<crate::vector_store::QdrantVectorStore>,
        llm_client: Arc<dyn LLMClient>,
        config: SyncConfig,
    ) -> Self {
        Self {
            filesystem,
            embedding,
            vector_store,
            llm_client,
            config,
        }
    }

    /// 创建默认配置的自动同步管理器
    pub fn with_defaults(
        filesystem: Arc<CortexFilesystem>,
        embedding: Arc<EmbeddingClient>,
        vector_store: Arc<QdrantVectorStore>,
        llm_client: Arc<dyn LLMClient>,
    ) -> Self {
        Self::new(filesystem, embedding, vector_store, llm_client, SyncConfig::default())
    }

    /// 同步所有内容到向量数据库
    pub async fn sync_all(&self) -> Result<SyncStats> {
        info!("Starting full sync to vector database");

        let mut total_stats = SyncStats::default();

        // 同步用户记忆 (preferences, entities, events)
        if self.config.sync_users {
            let stats = self
                .sync_directory("cortex://user", "L2")
                .await?;
            total_stats.add(&stats);
        }

        // 同步Agent记忆 (cases, skills)
        if self.config.sync_agents {
            let stats = self
                .sync_directory("cortex://agent", "L2")
                .await?;
            total_stats.add(&stats);
        }

        // 同步会话
        if self.config.sync_threads {
            let stats = self.sync_directory_recursive("cortex://session").await?;
            total_stats.add(&stats);
        }

        // 同步资源
        if self.config.sync_global {
            // resources目录可能不存在，如果存在则同步
            if let Ok(entries) = self.filesystem.list("cortex://resources").await {
                if !entries.is_empty() {
                    let stats = self
                        .sync_directory("cortex://resources", "L2")
                        .await?;
                    total_stats.add(&stats);
                }
            }
        }

        info!(
            "Sync completed: {} files processed, {} indexed, {} skipped, {} errors",
            total_stats.total_files,
            total_stats.indexed_files,
            total_stats.skipped_files,
            total_stats.error_files
        );

        Ok(total_stats)
    }

    /// 同步特定路径到向量数据库
    /// 
    /// 用于只索引特定session或特定路径的文件
    /// 例如: sync_specific_path("cortex://session/abc123")
    pub async fn sync_specific_path(&self, uri: &str) -> Result<SyncStats> {
        info!("Starting sync for specific path: {}", uri);

        // 检查路径是否存在
        if !self.filesystem.exists(uri).await? {
            warn!("Path does not exist: {}", uri);
            return Ok(SyncStats::default());
        }

        // 判断是session路径还是其他路径
        let stats = if uri.starts_with("cortex://session/") {
            // session路径使用递归同步（包含timeline等子目录）
            self.sync_directory_recursive(uri).await?
        } else if uri.starts_with("cortex://user/") || uri.starts_with("cortex://agent/") {
            // user/agent路径使用非递归同步
            self.sync_directory(uri, "L2").await?
        } else if uri.starts_with("cortex://resources/") {
            self.sync_directory(uri, "L2").await?
        } else {
            // 其他路径尝试递归同步
            self.sync_directory_recursive(uri).await?
        };

        info!(
            "Sync completed for {}: {} files processed, {} indexed, {} skipped, {} errors",
            uri,
            stats.total_files,
            stats.indexed_files,
            stats.skipped_files,
            stats.error_files
        );

        Ok(stats)
    }

    /// 同步单个目录（非递归）
    fn sync_directory<'a>(
        &'a self,
        uri: &'a str,
        layer: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SyncStats>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(uri).await?;
            let mut stats = SyncStats::default();

            for entry in entries {
                if entry.is_directory {
                    // 递归处理子目录
                    let sub_stats = self.sync_directory(&entry.uri, layer).await?;
                    stats.add(&sub_stats);
                } else if entry.name.ends_with(".md") {
                    // 处理Markdown文件
                    match self.sync_file(&entry.uri, layer).await {
                        Ok(true) => stats.indexed_files += 1,
                        Ok(false) => stats.skipped_files += 1,
                        Err(e) => {
                            warn!("Failed to sync {}: {}", entry.uri, e);
                            stats.error_files += 1;
                        }
                    }
                    stats.total_files += 1;
                }
            }

            Ok(stats)
        })
    }

    /// 同步目录(递归,用于threads)
    fn sync_directory_recursive<'a>(
        &'a self,
        uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SyncStats>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.list(uri).await?;
            let mut stats = SyncStats::default();

            // ✅ Generate timeline layers ONLY at session root level (not subdirectories)
            // This prevents overwriting session-level summaries with day-level summaries
            let is_session_timeline_root = uri.ends_with("/timeline") && !uri.contains("/timeline/");
            if is_session_timeline_root {
                if let Err(e) = self.generate_timeline_layers(uri).await {
                    warn!("Failed to generate timeline layers for {}: {}", uri, e);
                } else {
                    info!("Generated session-level timeline layers for {}", uri);
                }
            }

            for entry in entries {
                if entry.is_directory {
                    // 递归处理子目录
                    let sub_stats = self.sync_directory_recursive(&entry.uri).await?;
                    stats.add(&sub_stats);
                } else if entry.name.ends_with(".md") {
                    // 处理Markdown文件
                    match self.sync_file(&entry.uri, "L2").await {
                        Ok(true) => stats.indexed_files += 1,
                        Ok(false) => stats.skipped_files += 1,
                        Err(e) => {
                            warn!("Failed to sync {}: {}", entry.uri, e);
                            stats.error_files += 1;
                        }
                    }
                    stats.total_files += 1;
                }
            }

            Ok(stats)
        })
    }

    /// 同步单个文件（支持分层向量索引）
    async fn sync_file(&self, uri: &str, layer: &str) -> Result<bool> {
        // 检查是否已经索引（检查L2层）
        let l2_id = uri_to_vector_id(uri, ContextLayer::L2Detail);
        if self.is_indexed(&l2_id).await? {
            debug!("File already indexed: {}", uri);
            return Ok(false);
        }

        // 1. 读取并索引L2原始内容
        let l2_content = self.filesystem.read(uri).await?;
        let l2_embedding = self.embedding.embed(&l2_content).await?;
        let l2_metadata = self.parse_metadata(uri, layer)?;

        let l2_memory = Memory {
            id: l2_id.clone(),
            content: l2_content.clone(),
            embedding: l2_embedding,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: l2_metadata,
        };
        self.vector_store.insert(&l2_memory).await?;
        debug!("L2 indexed: {}", uri);

        // 2. 尝试读取并索引L0 abstract (目录级别)
        // 对于 timeline 文件，L0/L1 是目录级别的
        // 例如: cortex://session/abc/timeline/10_00.md 的 L0 是 cortex://session/abc/timeline/.abstract.md
        // 向量 ID 应该基于目录 URI: cortex://session/abc/timeline
        let (dir_uri, layer_file_uri) = Self::get_layer_info(uri, "L0");
        if let Ok(l0_content) = self.filesystem.read(&layer_file_uri).await {
            // 使用目录 URI 生成向量 ID，确保同一目录下的所有文件共享同一个 L0/L1 向量
            let l0_id = uri_to_vector_id(&dir_uri, ContextLayer::L0Abstract);
            if !self.is_indexed(&l0_id).await? {
                let l0_embedding = self.embedding.embed(&l0_content).await?;
                // 元数据使用目录 URI
                let l0_metadata = self.parse_metadata(&dir_uri, "L0")?;

                let l0_memory = Memory {
                    id: l0_id,
                    content: l0_content,
                    embedding: l0_embedding,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    metadata: l0_metadata,
                };
                self.vector_store.insert(&l0_memory).await?;
                debug!("L0 indexed for directory {}: {}", dir_uri, layer_file_uri);
            }
        }

        // 3. 尝试读取并索引L1 overview (目录级别)
        let (dir_uri, layer_file_uri) = Self::get_layer_info(uri, "L1");
        if let Ok(l1_content) = self.filesystem.read(&layer_file_uri).await {
            let l1_id = uri_to_vector_id(&dir_uri, ContextLayer::L1Overview);
            if !self.is_indexed(&l1_id).await? {
                let l1_embedding = self.embedding.embed(&l1_content).await?;
                let l1_metadata = self.parse_metadata(&dir_uri, "L1")?;

                let l1_memory = Memory {
                    id: l1_id,
                    content: l1_content,
                    embedding: l1_embedding,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    metadata: l1_metadata,
                };
                self.vector_store.insert(&l1_memory).await?;
                debug!("L1 indexed for directory {}: {}", dir_uri, layer_file_uri);
            }
        }

        Ok(true)
    }

    /// 获取分层信息 (目录 URI 和层文件 URI)
    /// 
    /// 对于 timeline 文件:
    /// - 输入: cortex://session/abc/timeline/10_00.md
    /// - L0 输出: (cortex://session/abc/timeline, cortex://session/abc/timeline/.abstract.md)
    /// 
    /// 对于 user/agent 记忆:
    /// - 输入: cortex://user/preferences/language.md
    /// - L0 输出: (cortex://user/preferences/language.md, cortex://user/preferences/.abstract.md)
    fn get_layer_info(file_uri: &str, layer: &str) -> (String, String) {
        let dir = file_uri
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(file_uri);
        
        let layer_file_uri = match layer {
            "L0" => format!("{}/.abstract.md", dir),
            "L1" => format!("{}/.overview.md", dir),
            _ => file_uri.to_string(),
        };
        
        // 目录 URI 用于向量 ID 生成
        // 对于 timeline，目录 URI 是文件所在的目录
        // 对于 user/agent，目录 URI 也是文件所在的目录
        (dir.to_string(), layer_file_uri)
    }

    /// 检查文件是否已索引
    async fn is_indexed(&self, id: &str) -> Result<bool> {
        // 尝试从向量数据库查询
        match self.vector_store.get(id).await {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => {
                debug!("Error checking if indexed: {}", e);
                Ok(false)
            }
        }
    }

    /// 解析URI获取元数据（支持layer标识）
    fn parse_metadata(
        &self,
        uri: &str,
        layer: &str,
    ) -> Result<MemoryMetadata> {
        use serde_json::Value;

        // 从URI中提取信息
        // 格式: cortex://dimension/path/to/file.md
        let parts: Vec<&str> = uri.split('/').collect();

        let (dimension, path): (&str, String) = if parts.len() >= 3 {
            (parts[2], parts[3..].join("/"))
        } else {
            (
                "session",
                uri.strip_prefix("cortex://").unwrap_or(uri).to_string(),
            )
        };

        let hash = self.calculate_hash(uri);

        let mut custom = std::collections::HashMap::new();
        custom.insert("uri".to_string(), Value::String(uri.to_string()));
        custom.insert("path".to_string(), Value::String(path.clone()));

        Ok(MemoryMetadata {
            uri: Some(uri.to_string()),
            user_id: if dimension == "user" {
                Some(path.clone())
            } else {
                None
            },
            agent_id: if dimension == "agent" {
                Some(path.clone())
            } else {
                None
            },
            run_id: if dimension == "session" {
                Some(path.clone())
            } else {
                None
            },
            actor_id: None,
            role: None,
            layer: layer.to_string(),
            hash,
            importance_score: 0.5,
            entities: vec![],
            topics: vec![],
            custom,
        })
    }

    /// 计算内容的哈希值
    fn calculate_hash(&self, content: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// 为timeline目录生成L0/L1层
    ///
    /// 调用LayerManager生成timeline级别的abstract和overview
    async fn generate_timeline_layers(&self, timeline_uri: &str) -> Result<()> {
        let layer_manager = LayerManager::new(self.filesystem.clone(), self.llm_client.clone());
        layer_manager.generate_timeline_layers(timeline_uri).await
    }
}

impl SyncStats {
    pub fn add(&mut self, other: &SyncStats) {
        self.total_files += other.total_files;
        self.indexed_files += other.indexed_files;
        self.skipped_files += other.skipped_files;
        self.error_files += other.error_files;
    }
}
