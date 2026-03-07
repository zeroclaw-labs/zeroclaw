use crate::{
    ContextLayer, Result,
    embedding::EmbeddingClient,
    filesystem::{CortexFilesystem, FilesystemOperations},
    session::Message,
    vector_store::{QdrantVectorStore, VectorStore},
};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// 自动索引管理器配置
#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// 是否自动索引新消息
    pub auto_index: bool,
    /// 批量索引的batch大小
    pub batch_size: usize,
    /// 是否在后台异步索引
    pub async_index: bool,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            auto_index: true,
            batch_size: 10,
            async_index: true,
        }
    }
}

/// 索引统计
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub total_indexed: usize,
    pub total_skipped: usize,
    pub total_errors: usize,
}

/// Timeline层索引统计
#[derive(Debug, Clone, Default)]
struct TimelineLayerStats {
    l0_indexed: usize,
    l1_indexed: usize,
    errors: usize,
}

/// 自动索引管理器
///
/// 负责：
/// 1. 监听新消息并自动生成embedding
/// 2. 批量索引现有消息
/// 3. 增量更新索引
pub struct AutoIndexer {
    filesystem: Arc<CortexFilesystem>,
    embedding: Arc<EmbeddingClient>,
    vector_store: Arc<QdrantVectorStore>,
    config: IndexerConfig,
}

impl AutoIndexer {
    /// 创建新的自动索引器
    pub fn new(
        filesystem: Arc<CortexFilesystem>,
        embedding: Arc<EmbeddingClient>,
        vector_store: Arc<QdrantVectorStore>,
        config: IndexerConfig,
    ) -> Self {
        Self {
            filesystem,
            embedding,
            vector_store,
            config,
        }
    }

    /// 索引单个消息
    pub async fn index_message(&self, thread_id: &str, message: &Message) -> Result<()> {
        info!("Indexing message {} in thread {}", message.id, thread_id);

        // 1. 生成embedding
        let embedding = self.embedding.embed(&message.content).await?;

        // 2. 创建Memory对象
        let uri = format!("cortex://session/{}/messages/{}", thread_id, message.id);
        let memory = crate::types::Memory {
            id: message.id.clone(),
            content: message.content.clone(),
            embedding,
            created_at: message.created_at,
            updated_at: message.created_at,
            metadata: crate::types::MemoryMetadata {
                uri: Some(uri),
                user_id: None,
                agent_id: None,
                run_id: Some(thread_id.to_string()),
                actor_id: None,
                role: Some(format!("{:?}", message.role)),
                layer: "L2".to_string(),
                hash: self.calculate_hash(&message.content),
                importance_score: 0.5,
                entities: vec![],
                topics: vec![],
                custom: std::collections::HashMap::new(),
            },
        };

        // 3. 存储到向量数据库
        self.vector_store.as_ref().insert(&memory).await?;

        debug!("Message {} indexed successfully", message.id);
        Ok(())
    }

    /// 批量索引线程中的所有消息
    pub async fn index_thread(&self, thread_id: &str) -> Result<IndexStats> {
        self.index_thread_with_progress::<fn(usize, usize)>(thread_id, None)
            .await
    }

    /// 批量索引线程中的所有消息，带进度回调
    pub async fn index_thread_with_progress<F>(
        &self,
        thread_id: &str,
        mut progress_callback: Option<F>,
    ) -> Result<IndexStats>
    where
        F: FnMut(usize, usize) + Send,
    {
        info!("Starting batch indexing for thread: {}", thread_id);

        let mut stats = IndexStats::default();

        // 1. 扫描timeline目录获取所有消息
        let messages = self.collect_messages(thread_id).await?;
        let total_messages = messages.len();
        info!("Found {} messages to index", total_messages);

        if total_messages == 0 {
            return Ok(stats);
        }

        // 2. 检查哪些消息已经被索引（通过查询向量数据库）
        let existing_ids = self.get_indexed_message_ids(thread_id).await?;
        let messages_to_index: Vec<_> = messages
            .into_iter()
            .filter(|m| !existing_ids.contains(&m.id))
            .collect();

        info!(
            "Skipping {} already indexed messages",
            total_messages - messages_to_index.len()
        );
        stats.total_skipped = total_messages - messages_to_index.len();

        if messages_to_index.is_empty() {
            info!("All messages already indexed");
            return Ok(stats);
        }

        // 3. 分批处理
        let total_to_index = messages_to_index.len();
        for (batch_idx, chunk) in messages_to_index.chunks(self.config.batch_size).enumerate() {
            let batch_start = batch_idx * self.config.batch_size;

            // 通知进度
            if let Some(ref mut callback) = progress_callback {
                callback(batch_start, total_to_index);
            }

            // 生成所有embedding
            let contents: Vec<String> = chunk.iter().map(|m| m.content.clone()).collect();

            match self.embedding.embed_batch(&contents).await {
                Ok(embeddings) => {
                    // 为每个消息创建Memory并存储
                    for (message, embedding) in chunk.iter().zip(embeddings.iter()) {
                        let uri = format!("cortex://session/{}/messages/{}", thread_id, message.id);
                        let memory = crate::types::Memory {
                            id: message.id.clone(),
                            content: message.content.clone(),
                            embedding: embedding.clone(),
                            created_at: message.created_at,
                            updated_at: message.created_at,
                            metadata: crate::types::MemoryMetadata {
                                uri: Some(uri),
                                user_id: None,
                                agent_id: None,
                                run_id: Some(thread_id.to_string()),
                                actor_id: None,
                                role: Some(format!("{:?}", message.role)),
                                layer: "L2".to_string(),
                                hash: self.calculate_hash(&message.content),
                                importance_score: 0.5,
                                entities: vec![],
                                topics: vec![],
                                custom: std::collections::HashMap::new(),
                            },
                        };

                        match self.vector_store.as_ref().insert(&memory).await {
                            Ok(_) => {
                                stats.total_indexed += 1;
                                debug!("Indexed message {}", message.id);
                            }
                            Err(e) => {
                                warn!("Failed to index message {}: {}", message.id, e);
                                stats.total_errors += 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to generate embeddings for batch {}: {}",
                        batch_idx, e
                    );
                    stats.total_errors += chunk.len();
                }
            }
        }

        info!(
            "Batch indexing complete: {} indexed, {} skipped, {} errors",
            stats.total_indexed, stats.total_skipped, stats.total_errors
        );

        // Index L0/L1 layers for timeline directories
        info!("Indexing timeline L0/L1 layers for thread: {}", thread_id);
        match self.index_timeline_layers(thread_id).await {
            Ok(layer_stats) => {
                info!(
                    "Timeline layers indexed: {} L0, {} L1",
                    layer_stats.l0_indexed, layer_stats.l1_indexed
                );
                stats.total_indexed += layer_stats.l0_indexed + layer_stats.l1_indexed;
                stats.total_errors += layer_stats.errors;
            }
            Err(e) => {
                warn!("Failed to index timeline layers: {}", e);
            }
        }

        Ok(stats)
    }

    /// 获取已索引的消息ID列表
    async fn get_indexed_message_ids(
        &self,
        thread_id: &str,
    ) -> Result<std::collections::HashSet<String>> {
        use crate::vector_store::VectorStore;

        // 使用scroll API获取所有已索引的消息ID
        let filters = crate::types::Filters {
            run_id: Some(thread_id.to_string()),
            ..Default::default()
        };

        // 滚动查询获取所有ID（不需要embedding）
        match self.vector_store.as_ref().scroll_ids(&filters, 1000).await {
            Ok(ids) => Ok(ids.into_iter().collect()),
            Err(e) => {
                warn!(
                    "Failed to get indexed message IDs: {}, assuming none indexed",
                    e
                );
                Ok(std::collections::HashSet::new())
            }
        }
    }

    /// 收集线程中的所有消息
    async fn collect_messages(&self, thread_id: &str) -> Result<Vec<Message>> {
        let timeline_uri = format!("cortex://session/{}/timeline", thread_id);
        let mut messages = Vec::new();

        self.collect_messages_recursive(&timeline_uri, &mut messages)
            .await?;

        Ok(messages)
    }

    /// 递归收集消息
    fn collect_messages_recursive<'a>(
        &'a self,
        uri: &'a str,
        messages: &'a mut Vec<Message>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.filesystem.as_ref().list(uri).await?;

            for entry in entries {
                if entry.is_directory && !entry.name.starts_with('.') {
                    self.collect_messages_recursive(&entry.uri, messages)
                        .await?;
                } else if entry.name.ends_with(".md") && !entry.name.starts_with('.') {
                    if let Ok(content) = self.filesystem.as_ref().read(&entry.uri).await {
                        // 先尝试解析为标准markdown格式
                        if let Some(message) = self.parse_message_markdown(&content) {
                            messages.push(message);
                        } else {
                            // 🔧 修复：从文件名正确提取message ID
                            // 文件名格式：HH_MM_SS_<uuid前8字符>.md
                            // 例如：15_10_18_28b538d8.md
                            // 但这只是UUID的前8字符，我们需要从文件内容中提取完整UUID

                            // 尝试从Markdown内容中手动提取ID（更宽松的解析）
                            let message_id = if let Some(id) =
                                Self::extract_id_from_content(&content)
                            {
                                id
                            } else {
                                // 如果仍然提取不到，尝试从文件名提取UUID部分
                                // 文件名格式：HH_MM_SS_xxxxxxxx.md，取最后一部分作为ID片段
                                let name_without_ext = entry.name.trim_end_matches(".md");
                                let parts: Vec<&str> = name_without_ext.split('_').collect();
                                if parts.len() >= 4 {
                                    // 取最后一个部分（UUID前8字符）
                                    // 但我们知道这不是完整UUID，所以给它一个警告
                                    let partial_id = parts[parts.len() - 1];
                                    warn!(
                                        "Could not extract full UUID from {}, using partial ID: {}",
                                        entry.uri, partial_id
                                    );
                                    // 跳过这个消息，因为部分ID无法用于向量存储
                                    continue;
                                } else {
                                    warn!("Invalid filename format: {}", entry.name);
                                    continue;
                                }
                            };

                            // 从entry.modified获取时间戳
                            let timestamp = entry.modified;

                            let message = Message {
                                id: message_id.clone(),                  // 🔧 clone以便后续使用
                                role: crate::session::MessageRole::User, // 默认为User
                                content: content.trim().to_string(),
                                timestamp,
                                created_at: timestamp,
                                metadata: None,
                            };

                            debug!(
                                "Collected message from {} with ID: {}",
                                entry.uri, message_id
                            );
                            messages.push(message);
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// 解析Markdown格式的消息
    fn parse_message_markdown(&self, content: &str) -> Option<Message> {
        use crate::session::MessageRole;

        let mut role = MessageRole::User;
        let mut message_content = String::new();
        let mut id = String::new();
        let mut timestamp = chrono::Utc::now();
        let mut in_content_section = false;

        for line in content.lines() {
            if line.starts_with("# 👤 User") || line.starts_with("# User") {
                role = MessageRole::User;
            } else if line.starts_with("# 🤖 Assistant") || line.starts_with("# Assistant") {
                role = MessageRole::Assistant;
            } else if line.starts_with("# ⚙️ System") || line.starts_with("# System") {
                role = MessageRole::System;
            } else if line.starts_with("**ID**:") {
                // 🔧 修复：更宽松地提取ID，支持多种格式
                if let Some(id_str) = line
                    .strip_prefix("**ID**:")
                    .map(|s| s.trim())
                    .and_then(|s| {
                        // 移除可能的`符号
                        s.trim_start_matches('`')
                            .trim_end_matches('`')
                            .trim()
                            .to_string()
                            .into()
                    })
                {
                    if !id_str.is_empty() {
                        id = id_str;
                    }
                }
            } else if line.starts_with("**Timestamp**:") {
                if let Some(ts_str) = line.strip_prefix("**Timestamp**:").map(|s| s.trim()) {
                    // 尝试多种时间格式
                    if let Ok(parsed_ts) =
                        chrono::DateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S %Z")
                    {
                        timestamp = parsed_ts.with_timezone(&chrono::Utc);
                    } else if let Ok(parsed_ts) =
                        chrono::DateTime::parse_from_str(ts_str, "%Y-%m-%d %H:%M:%S UTC")
                    {
                        timestamp = parsed_ts.with_timezone(&chrono::Utc);
                    }
                }
            } else if line.starts_with("## Content") {
                in_content_section = true;
            } else if line.starts_with("##") {
                // 其他section开始，内容section结束
                in_content_section = false;
            } else if in_content_section && !line.trim().is_empty() {
                if !message_content.is_empty() {
                    message_content.push('\n');
                }
                message_content.push_str(line);
            }
        }

        if !id.is_empty() && !message_content.is_empty() {
            Some(Message {
                id,
                role,
                content: message_content.trim().to_string(),
                timestamp,
                created_at: timestamp,
                metadata: None,
            })
        } else {
            None
        }
    }

    /// 🔧 新增：从Markdown内容中手动提取ID（更宽松的方式）
    fn extract_id_from_content(content: &str) -> Option<String> {
        for line in content.lines() {
            if line.contains("**ID**:") || line.contains("ID:") {
                // 尝试提取ID
                if let Some(id_part) = line.split(':').nth(1) {
                    let id = id_part.trim().trim_matches('`').trim().to_string();

                    // 验证是否是有效的UUID格式
                    if uuid::Uuid::parse_str(&id).is_ok() {
                        return Some(id);
                    }
                }
            }
        }
        None
    }

    /// 计算内容哈希
    fn calculate_hash(&self, content: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// 索引timeline目录的L0/L1层
    ///
    /// 该方法会递归扫描timeline目录结构，为每个包含.abstract.md和.overview.md的目录
    /// 生成L0/L1层的向量索引
    async fn index_timeline_layers(&self, thread_id: &str) -> Result<TimelineLayerStats> {
        let mut stats = TimelineLayerStats::default();
        let timeline_base = format!("cortex://session/{}/timeline", thread_id);

        // 递归收集所有timeline目录
        let directories = self.collect_timeline_directories(&timeline_base).await?;
        info!("Found {} timeline directories to index", directories.len());

        for dir_uri in directories {
            // 索引L0 Abstract
            let l0_file_uri = format!("{}/.abstract.md", dir_uri);
            if let Ok(l0_content) = self.filesystem.as_ref().read(&l0_file_uri).await {
                match self
                    .index_layer(&dir_uri, &l0_content, ContextLayer::L0Abstract)
                    .await
                {
                    Ok(indexed) => {
                        if indexed {
                            stats.l0_indexed += 1;
                            debug!("Indexed L0 for {}", dir_uri);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to index L0 for {}: {}", dir_uri, e);
                        stats.errors += 1;
                    }
                }
            }

            // 索引L1 Overview
            let l1_file_uri = format!("{}/.overview.md", dir_uri);
            if let Ok(l1_content) = self.filesystem.as_ref().read(&l1_file_uri).await {
                match self
                    .index_layer(&dir_uri, &l1_content, ContextLayer::L1Overview)
                    .await
                {
                    Ok(indexed) => {
                        if indexed {
                            stats.l1_indexed += 1;
                            debug!("Indexed L1 for {}", dir_uri);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to index L1 for {}: {}", dir_uri, e);
                        stats.errors += 1;
                    }
                }
            }
        }

        Ok(stats)
    }

    /// 收集timeline目录结构中的所有目录URI
    async fn collect_timeline_directories(&self, base_uri: &str) -> Result<Vec<String>> {
        let mut directories = Vec::new();
        self.collect_directories_recursive(base_uri, &mut directories)
            .await?;
        Ok(directories)
    }

    /// 递归收集目录
    fn collect_directories_recursive<'a>(
        &'a self,
        uri: &'a str,
        directories: &'a mut Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            match self.filesystem.as_ref().list(uri).await {
                Ok(entries) => {
                    // 检查当前目录是否包含.abstract.md或.overview.md
                    let has_layers = entries
                        .iter()
                        .any(|e| e.name == ".abstract.md" || e.name == ".overview.md");

                    if has_layers {
                        directories.push(uri.to_string());
                    }

                    // 递归处理子目录
                    for entry in entries {
                        if entry.is_directory && !entry.name.starts_with('.') {
                            self.collect_directories_recursive(&entry.uri, directories)
                                .await?;
                        }
                    }
                    Ok(())
                }
                Err(e) => {
                    debug!("Failed to list {}: {}", uri, e);
                    Ok(())
                }
            }
        })
    }

    /// 索引单个层（L0或L1）
    ///
    /// 返回: Ok(true)表示已索引, Ok(false)表示已存在跳过
    async fn index_layer(&self, dir_uri: &str, content: &str, layer: ContextLayer) -> Result<bool> {
        use crate::vector_store::{VectorStore, uri_to_vector_id};

        // 生成向量ID（基于目录URI，不是文件URI）
        let vector_id = uri_to_vector_id(dir_uri, layer);

        // 检查是否已索引
        if let Ok(Some(_)) = self.vector_store.as_ref().get(&vector_id).await {
            debug!("Layer {:?} already indexed for {}", layer, dir_uri);
            return Ok(false);
        }

        // 生成embedding
        let embedding = self.embedding.embed(content).await?;

        // 创建Memory对象
        let memory = crate::types::Memory {
            id: vector_id,
            content: content.to_string(),
            embedding,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: crate::types::MemoryMetadata {
                uri: Some(dir_uri.to_string()), // 关键：存储目录URI而非文件URI
                user_id: None,
                agent_id: None,
                run_id: None,
                actor_id: None,
                role: None,
                layer: match layer {
                    ContextLayer::L0Abstract => "L0",
                    ContextLayer::L1Overview => "L1",
                    ContextLayer::L2Detail => "L2",
                }.to_string(),
                hash: self.calculate_hash(content),
                importance_score: 0.5,
                entities: vec![],
                topics: vec![],
                custom: std::collections::HashMap::new(),
            },
        };

        // 存储到Qdrant
        self.vector_store.as_ref().insert(&memory).await?;
        Ok(true)
    }
}
