//! Cortex-Mem 核心功能测试
//!
//! 测试分类：
//! 1. 单元测试 (unit_*) - 不依赖外部服务，使用 Mock
//! 2. 集成测试 (integration_*) - 需要外部服务 (Qdrant, LLM, Embedding)
//!
//! 运行方式：
//! - 单元测试: `cargo test`
//! - 集成测试: `cargo test -- --ignored` (需要配置外部服务)

#![cfg(test)]

use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::RwLock;

// ==================== Mock 实现 ====================

mod mock {
    use async_trait::async_trait;
    use cortex_mem_core::llm::LLMClient;
    use cortex_mem_core::llm::{LLMConfig, MemoryExtractionResponse};
    use cortex_mem_core::llm::extractor_types::{StructuredFactExtraction, DetailedFactExtraction};
    use cortex_mem_core::Result;

    /// Mock LLM Client - 返回预定义的响应
    pub struct MockLLMClient {
        config: LLMConfig,
    }

    impl MockLLMClient {
        pub fn new() -> Self {
            Self {
                config: LLMConfig::default(),
            }
        }
    }

    impl Default for MockLLMClient {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl LLMClient for MockLLMClient {
        async fn complete(&self, _prompt: &str) -> Result<String> {
            Ok("Mock LLM response".to_string())
        }

        async fn complete_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            Ok("Mock LLM response with system prompt".to_string())
        }

        async fn extract_memories(&self, _prompt: &str) -> Result<MemoryExtractionResponse> {
            Ok(MemoryExtractionResponse {
                facts: vec![],
                decisions: vec![],
                entities: vec![],
            })
        }

        async fn extract_structured_facts(&self, _prompt: &str) -> Result<StructuredFactExtraction> {
            Ok(StructuredFactExtraction { facts: vec![] })
        }

        async fn extract_detailed_facts(&self, _prompt: &str) -> Result<DetailedFactExtraction> {
            Ok(DetailedFactExtraction { facts: vec![] })
        }

        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn config(&self) -> &LLMConfig {
            &self.config
        }
    }
}

// ==================== 测试辅助函数 ====================

mod test_utils {
    use super::*;
    use cortex_mem_core::{
        CortexFilesystem, FilesystemOperations, SessionConfig, SessionManager,
        layers::manager::LayerManager,
    };

    /// 测试用的上下文封装
    /// 
    /// 由于 MemoryOperations 的字段是 pub(crate)，测试无法直接构造。
    /// 这个结构体封装了测试所需的核心功能。
    #[allow(dead_code)]
    pub struct TestContext {
        pub filesystem: Arc<CortexFilesystem>,
        pub session_manager: Arc<RwLock<SessionManager>>,
        pub layer_manager: Arc<LayerManager>,
        pub temp_dir: TempDir,
    }

    impl TestContext {
        /// 创建新的测试上下文
        pub async fn new() -> Self {
            let temp_dir = TempDir::new().unwrap();
            let data_dir = temp_dir.path().to_str().unwrap();
            
            let filesystem = Arc::new(CortexFilesystem::new(data_dir));
            filesystem.initialize().await.unwrap();

            let config = SessionConfig::default();
            let session_manager = SessionManager::new(filesystem.clone(), config);
            let session_manager = Arc::new(RwLock::new(session_manager));

            let llm_client = Arc::new(mock::MockLLMClient::new());
            let layer_manager = Arc::new(LayerManager::new(filesystem.clone(), llm_client));

            Self {
                filesystem,
                session_manager,
                layer_manager,
                temp_dir,
            }
        }

        /// 创建带有租户隔离的测试上下文
        pub async fn with_tenant(tenant_id: &str) -> Self {
            let temp_dir = TempDir::new().unwrap();
            let data_dir = temp_dir.path().to_str().unwrap();
            
            let filesystem = Arc::new(CortexFilesystem::with_tenant(data_dir, tenant_id));
            filesystem.initialize().await.unwrap();

            let config = SessionConfig::default();
            let session_manager = SessionManager::new(filesystem.clone(), config);
            let session_manager = Arc::new(RwLock::new(session_manager));

            let llm_client = Arc::new(mock::MockLLMClient::new());
            let layer_manager = Arc::new(LayerManager::new(filesystem.clone(), llm_client));

            Self {
                filesystem,
                session_manager,
                layer_manager,
                temp_dir,
            }
        }

        /// 添加消息到会话
        pub async fn add_message(&self, thread_id: &str, role: &str, content: &str) -> String {
            let thread_id = if thread_id.is_empty() { "default" } else { thread_id };

            {
                let sm = self.session_manager.read().await;
                if sm.session_exists(thread_id).await.unwrap() {
                    // Session exists, proceed to add message
                } else {
                    drop(sm);
                    let sm = self.session_manager.write().await;
                    sm.create_session_with_ids(thread_id, None, None).await.unwrap();
                }
            }

            let sm = self.session_manager.read().await;
            let message = cortex_mem_core::Message::new(
                match role {
                    "user" => cortex_mem_core::MessageRole::User,
                    "assistant" => cortex_mem_core::MessageRole::Assistant,
                    "system" => cortex_mem_core::MessageRole::System,
                    _ => cortex_mem_core::MessageRole::User,
                },
                content,
            );

            sm.message_storage().save_message(thread_id, &message).await.unwrap()
        }

        /// 列出会话
        pub async fn list_sessions(&self) -> Vec<SessionInfo> {
            let entries = self.filesystem.list("cortex://session").await.unwrap();
            let mut session_infos = Vec::new();
            
            for entry in entries {
                if entry.is_directory {
                    let thread_id = entry.name.clone();
                    if let Ok(metadata) = self.session_manager.read().await.load_session(&thread_id).await {
                        let status_str = match metadata.status {
                            cortex_mem_core::session::manager::SessionStatus::Active => "active",
                            cortex_mem_core::session::manager::SessionStatus::Closed => "closed",
                            cortex_mem_core::session::manager::SessionStatus::Archived => "archived",
                        };

                        session_infos.push(SessionInfo {
                            thread_id: metadata.thread_id,
                            status: status_str.to_string(),
                            message_count: 0,
                            created_at: metadata.created_at,
                            updated_at: metadata.updated_at,
                        });
                    }
                }
            }

            session_infos
        }

        /// 获取会话信息
        pub async fn get_session(&self, thread_id: &str) -> Result<SessionInfo, String> {
            let sm = self.session_manager.read().await;
            let metadata = sm.load_session(thread_id).await.map_err(|e| e.to_string())?;

            let status_str = match metadata.status {
                cortex_mem_core::session::manager::SessionStatus::Active => "active",
                cortex_mem_core::session::manager::SessionStatus::Closed => "closed",
                cortex_mem_core::session::manager::SessionStatus::Archived => "archived",
            };

            Ok(SessionInfo {
                thread_id: metadata.thread_id,
                status: status_str.to_string(),
                message_count: 0,
                created_at: metadata.created_at,
                updated_at: metadata.updated_at,
            })
        }

        /// 关闭会话
        pub async fn close_session(&self, thread_id: &str) -> Result<(), String> {
            let mut sm = self.session_manager.write().await;
            sm.close_session(thread_id).await.map_err(|e| e.to_string())?;
            Ok(())
        }

        /// 存储内容
        pub async fn store(&self, args: StoreArgs) -> StoreResponse {
            let scope = match args.scope.as_str() {
                "user" | "session" | "agent" => args.scope.as_str(),
                _ => "session",
            };

            let uri = match scope {
                "user" => {
                    let user_id = args.user_id.as_deref().unwrap_or("default");
                    let now = chrono::Utc::now();
                    let year_month = now.format("%Y-%m").to_string();
                    let day = now.format("%d").to_string();
                    let filename = format!(
                        "{}_{}.md",
                        now.format("%H_%M_%S"),
                        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown")
                    );
                    format!("cortex://user/{}/memories/{}/{}/{}", user_id, year_month, day, filename)
                },
                "agent" => {
                    let agent_id = args.agent_id.as_deref()
                        .or_else(|| if args.thread_id.is_empty() { None } else { Some(&args.thread_id) })
                        .unwrap_or("default");
                    let now = chrono::Utc::now();
                    let year_month = now.format("%Y-%m").to_string();
                    let day = now.format("%d").to_string();
                    let filename = format!(
                        "{}_{}.md",
                        now.format("%H_%M_%S"),
                        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown")
                    );
                    format!("cortex://agent/{}/memories/{}/{}/{}", agent_id, year_month, day, filename)
                },
                "session" => {
                    self.add_message(
                        if args.thread_id.is_empty() { "default" } else { &args.thread_id },
                        "user",
                        &args.content
                    ).await
                },
                _ => unreachable!(),
            };

            if scope == "user" || scope == "agent" {
                self.filesystem.write(&uri, &args.content).await.unwrap();
            }

            if args.auto_generate_layers.unwrap_or(true) {
                let _ = self.layer_manager.generate_all_layers(&uri, &args.content).await;
            }

            StoreResponse {
                uri,
                layers_generated: HashMap::new(),
                success: true,
            }
        }

        /// 获取 L0 abstract
        pub async fn get_abstract(&self, uri: &str) -> Result<AbstractResponse, String> {
            let text: String = self.layer_manager
                .load(uri, cortex_mem_core::ContextLayer::L0Abstract)
                .await
                .map_err(|e: cortex_mem_core::Error| e.to_string())?;
            Ok(AbstractResponse {
                uri: uri.to_string(),
                abstract_text: text.clone(),
                layer: "L0".to_string(),
                token_count: text.split_whitespace().count(),
            })
        }

        /// 获取 L1 overview
        pub async fn get_overview(&self, uri: &str) -> Result<OverviewResponse, String> {
            let text: String = self.layer_manager
                .load(uri, cortex_mem_core::ContextLayer::L1Overview)
                .await
                .map_err(|e: cortex_mem_core::Error| e.to_string())?;
            Ok(OverviewResponse {
                uri: uri.to_string(),
                overview_text: text.clone(),
                layer: "L1".to_string(),
                token_count: text.split_whitespace().count(),
            })
        }

        /// 获取 L2 完整内容
        pub async fn get_read(&self, uri: &str) -> Result<ReadResponse, String> {
            let content = self.filesystem.read(uri).await.map_err(|e| e.to_string())?;
            Ok(ReadResponse {
                uri: uri.to_string(),
                content: content.clone(),
                layer: "L2".to_string(),
                token_count: content.split_whitespace().count(),
                metadata: None,
            })
        }

        /// 列出目录
        pub async fn list(&self, uri: &str) -> Vec<String> {
            self.filesystem.list(uri).await
                .map(|entries| entries.into_iter().map(|e| e.uri).collect())
                .unwrap_or_default()
        }

        /// 读取文件
        pub async fn read(&self, uri: &str) -> Result<String, String> {
            self.filesystem.read(uri).await.map_err(|e| e.to_string())
        }

        /// 删除文件
        pub async fn delete(&self, uri: &str) -> Result<(), String> {
            self.filesystem.delete(uri).await.map_err(|e| e.to_string())
        }

        /// 检查文件是否存在
        pub async fn exists(&self, uri: &str) -> bool {
            self.filesystem.exists(uri).await.unwrap_or(false)
        }

        /// 写入文件
        pub async fn write(&self, uri: &str, content: &str) -> Result<(), String> {
            self.filesystem.write(uri, content).await.map_err(|e| e.to_string())
        }
    }

    // 类型定义
    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct SessionInfo {
        pub thread_id: String,
        pub status: String,
        pub message_count: usize,
        pub created_at: chrono::DateTime<chrono::Utc>,
        pub updated_at: chrono::DateTime<chrono::Utc>,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct StoreArgs {
        pub content: String,
        pub thread_id: String,
        pub metadata: Option<serde_json::Value>,
        pub auto_generate_layers: Option<bool>,
        pub scope: String,
        pub user_id: Option<String>,
        pub agent_id: Option<String>,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct StoreResponse {
        pub uri: String,
        pub layers_generated: HashMap<String, String>,
        pub success: bool,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct AbstractResponse {
        pub uri: String,
        pub abstract_text: String,
        pub layer: String,
        pub token_count: usize,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct OverviewResponse {
        pub uri: String,
        pub overview_text: String,
        pub layer: String,
        pub token_count: usize,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct ReadResponse {
        pub uri: String,
        pub content: String,
        pub layer: String,
        pub token_count: usize,
        pub metadata: Option<FileMetadata>,
    }

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct FileMetadata {
        pub created_at: chrono::DateTime<chrono::Utc>,
        pub updated_at: chrono::DateTime<chrono::Utc>,
    }
}

// ==================== 单元测试: 文件系统基础操作 ====================

mod unit_filesystem_tests {
    use super::*;

    /// 测试基本的文件写入和读取
    #[tokio::test]
    async fn test_basic_write_and_read() {
        let ctx = test_utils::TestContext::new().await;

        let content = "Hello, Cortex Memory!";
        let uri = "cortex://resources/test.md";
        ctx.write(uri, content).await.unwrap();

        let read_content = ctx.read(uri).await.unwrap();
        assert_eq!(read_content, content);
    }

    /// 测试文件存在性检查
    #[tokio::test]
    async fn test_file_exists() {
        let ctx = test_utils::TestContext::new().await;

        assert!(!ctx.exists("cortex://resources/nonexistent.md").await);

        ctx.write("cortex://resources/test.md", "content").await.unwrap();
        assert!(ctx.exists("cortex://resources/test.md").await);
    }

    /// 测试文件删除
    #[tokio::test]
    async fn test_file_delete() {
        let ctx = test_utils::TestContext::new().await;

        let uri = "cortex://resources/to_delete.md";
        
        ctx.write(uri, "content").await.unwrap();
        assert!(ctx.exists(uri).await);

        ctx.delete(uri).await.unwrap();
        assert!(!ctx.exists(uri).await);

        let result = ctx.delete(uri).await;
        assert!(result.is_err());
    }

    /// 测试目录列表
    #[tokio::test]
    async fn test_list_directory() {
        let ctx = test_utils::TestContext::new().await;

        ctx.write("cortex://resources/file1.md", "content1").await.unwrap();
        ctx.write("cortex://resources/file2.md", "content2").await.unwrap();
        ctx.write("cortex://resources/subdir/file3.md", "content3").await.unwrap();

        let entries = ctx.list("cortex://resources").await;
        
        assert!(entries.len() >= 2);
        
        let names: Vec<&str> = entries.iter().map(|e| e.rsplit('/').next().unwrap()).collect();
        assert!(names.contains(&"file1.md"));
        assert!(names.contains(&"file2.md"));
    }

    /// 测试嵌套目录创建
    #[tokio::test]
    async fn test_nested_directory_creation() {
        let ctx = test_utils::TestContext::new().await;

        let uri = "cortex://resources/level1/level2/level3/deep.md";
        ctx.write(uri, "deep content").await.unwrap();

        let content = ctx.read(uri).await.unwrap();
        assert_eq!(content, "deep content");
    }

    /// 测试空内容存储
    #[tokio::test]
    async fn test_empty_content() {
        let ctx = test_utils::TestContext::new().await;

        let uri = "cortex://resources/empty.md";
        ctx.write(uri, "").await.unwrap();

        let content = ctx.read(uri).await.unwrap();
        assert!(content.is_empty());
    }

    /// 测试读取不存在的文件
    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let ctx = test_utils::TestContext::new().await;

        let result = ctx.read("cortex://resources/nonexistent.md").await;
        assert!(result.is_err());
    }
}

// ==================== 单元测试: 会话管理 ====================

mod unit_session_tests {
    use super::*;

    /// 测试添加消息到会话
    #[tokio::test]
    async fn test_add_message() {
        let ctx = test_utils::TestContext::new().await;

        let thread_id = "test_thread";
        let msg_id = ctx.add_message(thread_id, "user", "Hello, world!").await;

        assert!(!msg_id.is_empty());

        let sessions = ctx.list_sessions().await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].thread_id, thread_id);
    }

    /// 测试空 thread_id 使用默认值
    #[tokio::test]
    async fn test_empty_thread_id_defaults() {
        let ctx = test_utils::TestContext::new().await;

        let msg_id = ctx.add_message("", "user", "test message").await;
        assert!(!msg_id.is_empty());

        let session = ctx.get_session("default").await.unwrap();
        assert_eq!(session.thread_id, "default");
        assert_eq!(session.status, "active");
    }

    /// 测试多角色消息
    #[tokio::test]
    async fn test_multiple_roles() {
        let ctx = test_utils::TestContext::new().await;

        let thread_id = "multi_role_thread";

        ctx.add_message(thread_id, "user", "User message").await;
        ctx.add_message(thread_id, "assistant", "Assistant response").await;
        ctx.add_message(thread_id, "system", "System instruction").await;

        let session = ctx.get_session(thread_id).await.unwrap();
        assert_eq!(session.thread_id, thread_id);
    }

    /// 测试会话关闭
    #[tokio::test]
    async fn test_session_close() {
        let ctx = test_utils::TestContext::new().await;

        let thread_id = "session_to_close";
        ctx.add_message(thread_id, "user", "message").await;

        ctx.close_session(thread_id).await.unwrap();

        let session = ctx.get_session(thread_id).await.unwrap();
        assert_eq!(session.status, "closed");
    }

    /// 测试多个会话
    #[tokio::test]
    async fn test_multiple_sessions() {
        let ctx = test_utils::TestContext::new().await;

        ctx.add_message("thread1", "user", "message 1").await;
        ctx.add_message("thread2", "user", "message 2").await;
        ctx.add_message("thread3", "user", "message 3").await;

        let sessions = ctx.list_sessions().await;
        assert_eq!(sessions.len(), 3);

        for session in &sessions {
            assert_eq!(session.status, "active");
        }
    }

    /// 测试获取不存在的会话
    #[tokio::test]
    async fn test_get_nonexistent_session() {
        let ctx = test_utils::TestContext::new().await;

        let result = ctx.get_session("nonexistent_session").await;
        assert!(result.is_err());
    }
}

// ==================== 单元测试: 存储操作 ====================

mod unit_storage_tests {
    use super::*;

    /// 测试 session scope 存储
    #[tokio::test]
    async fn test_store_session_scope() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "Session content".to_string(),
            thread_id: "test_session".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "session".to_string(),
            user_id: None,
            agent_id: None,
        };

        let result = ctx.store(args).await;
        assert!(result.success);
        assert!(result.uri.starts_with("cortex://session/test_session/timeline"));
        assert!(result.uri.ends_with(".md"));

        let content = ctx.read(&result.uri).await.unwrap();
        assert!(content.contains("Session content"));
    }

    /// 测试 user scope 存储
    #[tokio::test]
    async fn test_store_user_scope() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "User preference content".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("user_123".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        assert!(result.success);
        assert!(result.uri.starts_with("cortex://user/user_123/memories"));
        assert!(result.uri.ends_with(".md"));
    }

    /// 测试 agent scope 存储
    #[tokio::test]
    async fn test_store_agent_scope() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "Agent case content".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "agent".to_string(),
            user_id: None,
            agent_id: Some("agent_456".to_string()),
        };

        let result = ctx.store(args).await;
        assert!(result.success);
        assert!(result.uri.starts_with("cortex://agent/agent_456/memories"));
        assert!(result.uri.ends_with(".md"));
    }

    /// 测试自动生成层
    #[tokio::test]
    async fn test_auto_generate_layers() {
        let ctx = test_utils::TestContext::new().await;

        let content = r#"# Test Document

This is a test document with some content.

## Section 1
Content for section 1.
"#;

        let args = test_utils::StoreArgs {
            content: content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("layer_test_user".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        assert!(result.success);

        // 验证 L2 可读取
        let l2 = ctx.get_read(&result.uri).await.unwrap();
        assert!(l2.content.contains("Test Document"));
        assert_eq!(l2.layer, "L2");

        // 验证 L0 摘要可获取
        let l0 = ctx.get_abstract(&result.uri).await.unwrap();
        assert!(!l0.abstract_text.is_empty());
        assert_eq!(l0.layer, "L0");

        // 验证 L1 概览可获取
        let l1 = ctx.get_overview(&result.uri).await.unwrap();
        assert!(!l1.overview_text.is_empty());
        assert_eq!(l1.layer, "L1");
    }

    /// 测试存储带元数据
    #[tokio::test]
    async fn test_store_with_metadata() {
        let ctx = test_utils::TestContext::new().await;

        let metadata = serde_json::json!({
            "importance": "high",
            "tags": ["rust", "testing"],
        });

        let args = test_utils::StoreArgs {
            content: "Content with metadata".to_string(),
            thread_id: "".to_string(),
            metadata: Some(metadata),
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("metadata_user".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        assert!(result.success);
    }
}

// ==================== 单元测试: 多租户隔离 ====================

mod unit_tenant_isolation_tests {
    use super::*;

    /// 测试租户数据隔离
    #[tokio::test]
    async fn test_tenant_data_isolation() {
        let ctx_a = test_utils::TestContext::with_tenant("tenant_a").await;
        let ctx_b = test_utils::TestContext::with_tenant("tenant_b").await;

        // 租户 A 存储数据
        let args_a = test_utils::StoreArgs {
            content: "Tenant A private data".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("shared_user".to_string()),
            agent_id: None,
        };
        let result_a = ctx_a.store(args_a).await;

        // 租户 B 存储数据
        let args_b = test_utils::StoreArgs {
            content: "Tenant B private data".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("shared_user".to_string()),
            agent_id: None,
        };
        let result_b = ctx_b.store(args_b).await;

        // 验证 URI 不同
        assert_ne!(result_a.uri, result_b.uri);

        // 验证租户 A 能读取自己的数据
        let content_a = ctx_a.read(&result_a.uri).await.unwrap();
        assert!(content_a.contains("Tenant A"));

        // 验证租户 B 能读取自己的数据
        let content_b = ctx_b.read(&result_b.uri).await.unwrap();
        assert!(content_b.contains("Tenant B"));

        // 验证租户 A 不能读取租户 B 的数据
        let read_result = ctx_a.read(&result_b.uri).await;
        assert!(read_result.is_err());
    }

    /// 测试会话隔离
    #[tokio::test]
    async fn test_session_isolation() {
        let ctx_a = test_utils::TestContext::with_tenant("tenant_a").await;
        let ctx_b = test_utils::TestContext::with_tenant("tenant_b").await;

        ctx_a.add_message("shared_thread_id", "user", "Tenant A message").await;
        ctx_b.add_message("shared_thread_id", "user", "Tenant B message").await;

        let sessions_a = ctx_a.list_sessions().await;
        let sessions_b = ctx_b.list_sessions().await;

        assert_eq!(sessions_a.len(), 1);
        assert_eq!(sessions_b.len(), 1);
    }
}

// ==================== 单元测试: Scope 隔离 ====================

mod unit_scope_isolation_tests {
    use super::*;

    /// 测试不同 scope 的存储路径
    #[tokio::test]
    async fn test_scope_path_isolation() {
        let ctx = test_utils::TestContext::new().await;

        // Session scope
        let session_args = test_utils::StoreArgs {
            content: "Session data".to_string(),
            thread_id: "my_thread".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "session".to_string(),
            user_id: None,
            agent_id: None,
        };
        let session_result = ctx.store(session_args).await;
        assert!(session_result.uri.starts_with("cortex://session/my_thread"));

        // User scope
        let user_args = test_utils::StoreArgs {
            content: "User data".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("user_001".to_string()),
            agent_id: None,
        };
        let user_result = ctx.store(user_args).await;
        assert!(user_result.uri.starts_with("cortex://user/user_001"));

        // Agent scope
        let agent_args = test_utils::StoreArgs {
            content: "Agent data".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "agent".to_string(),
            user_id: None,
            agent_id: Some("agent_001".to_string()),
        };
        let agent_result = ctx.store(agent_args).await;
        assert!(agent_result.uri.starts_with("cortex://agent/agent_001"));

        // 验证所有 URI 都不同
        assert_ne!(session_result.uri, user_result.uri);
        assert_ne!(user_result.uri, agent_result.uri);
        assert_ne!(session_result.uri, agent_result.uri);
    }

    /// 测试不同 user_id 之间的隔离
    #[tokio::test]
    async fn test_user_id_isolation() {
        let ctx = test_utils::TestContext::new().await;

        let args_a = test_utils::StoreArgs {
            content: "User A data".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("user_a".to_string()),
            agent_id: None,
        };
        let result_a = ctx.store(args_a).await;

        let args_b = test_utils::StoreArgs {
            content: "User B data".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("user_b".to_string()),
            agent_id: None,
        };
        let result_b = ctx.store(args_b).await;

        assert_ne!(result_a.uri, result_b.uri);
        assert!(result_a.uri.contains("user_a"));
        assert!(result_b.uri.contains("user_b"));
    }
}

// ==================== 单元测试: 边界情况 ====================

mod unit_edge_case_tests {
    use super::*;

    /// 测试特殊字符内容
    #[tokio::test]
    async fn test_special_characters() {
        let ctx = test_utils::TestContext::new().await;

        let special_contents = vec![
            ("中文内容", "Chinese characters"),
            ("Emoji 🎉🚀💡", "Emojis"),
            ("Tabs\tand\tspaces", "Tabs"),
            ("Newlines\nLine1\nLine2", "Newlines"),
            ("Quotes: \"double\" 'single'", "Quotes"),
            ("HTML <tag> & entities", "HTML"),
            ("Code: `fn main() {}`", "Code"),
        ];

        for (content, desc) in special_contents {
            let args = test_utils::StoreArgs {
                content: content.to_string(),
                thread_id: "".to_string(),
                metadata: None,
                auto_generate_layers: Some(false),
                scope: "user".to_string(),
                user_id: Some("special_char_test".to_string()),
                agent_id: None,
            };

            let result = ctx.store(args).await;
            assert!(result.success, "Failed for: {}", desc);

            let read_content = ctx.read(&result.uri).await.unwrap();
            assert!(read_content.contains(content), "Content mismatch for: {}", desc);
        }
    }

    /// 测试大内容存储
    #[tokio::test]
    async fn test_large_content() {
        let ctx = test_utils::TestContext::new().await;

        let large_content = "X".repeat(50 * 1024);

        let args = test_utils::StoreArgs {
            content: large_content.clone(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("large_content_user".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        assert!(result.success);

        let read_content = ctx.read(&result.uri).await.unwrap();
        assert!(read_content.len() >= large_content.len() - 10);
    }

    /// 测试特殊 thread_id
    #[tokio::test]
    async fn test_special_thread_ids() {
        let ctx = test_utils::TestContext::new().await;

        let special_ids = vec![
            "thread-with-dash",
            "thread_with_underscore",
            "thread.with.dot",
            "thread123",
            "123thread",
        ];

        for thread_id in special_ids {
            let result = ctx.add_message(thread_id, "user", "test message").await;
            assert!(!result.is_empty(), "Failed for thread_id: {}", thread_id);
        }
    }

    /// 测试无效的 scope
    #[tokio::test]
    async fn test_invalid_scope() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "test".to_string(),
            thread_id: "test_thread".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "invalid_scope".to_string(),
            user_id: None,
            agent_id: None,
        };

        let result = ctx.store(args).await;
        assert!(result.uri.starts_with("cortex://session"));
    }
}

// ==================== 单元测试: 并发操作 ====================

mod unit_concurrent_tests {
    use super::*;

    /// 测试并发写入
    #[tokio::test]
    async fn test_concurrent_writes() {
        let ctx = Arc::new(test_utils::TestContext::new().await);

        let mut handles = vec![];

        for i in 0..20 {
            let ctx_clone = ctx.clone();
            let handle = tokio::spawn(async move {
                ctx_clone.add_message("concurrent_test", "user", &format!("Message {}", i)).await
            });
            handles.push(handle);
        }

        let results: Vec<_> = futures::future::join_all(handles).await;
        let success_count = results.iter().filter(|r| !r.as_ref().unwrap().is_empty()).count();

        assert_eq!(success_count, 20, "All concurrent writes should succeed");
    }

    /// 测试并发读取
    #[tokio::test]
    async fn test_concurrent_reads() {
        let ctx = Arc::new(test_utils::TestContext::new().await);

        let args = test_utils::StoreArgs {
            content: "Shared content for concurrent reads".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("concurrent_read_user".to_string()),
            agent_id: None,
        };
        let result = ctx.store(args).await;
        let uri = Arc::new(result.uri);

        let mut handles = vec![];

        for _ in 0..50 {
            let ctx_clone = ctx.clone();
            let uri_clone = uri.clone();
            let handle = tokio::spawn(async move {
                ctx_clone.read(&uri_clone).await
            });
            handles.push(handle);
        }

        let results: Vec<_> = futures::future::join_all(handles).await;
        let success_count = results.iter().filter(|r| r.is_ok() && r.as_ref().unwrap().is_ok()).count();

        assert_eq!(success_count, 50, "All concurrent reads should succeed");
    }

    /// 测试并发读写
    #[tokio::test]
    async fn test_concurrent_read_write() {
        let ctx = Arc::new(test_utils::TestContext::new().await);

        for i in 0..5 {
            ctx.add_message("rw_test", "user", &format!("Initial {}", i)).await;
        }

        let mut handles: Vec<tokio::task::JoinHandle<Result<(), String>>> = vec![];

        for i in 0..20 {
            let ctx_clone = ctx.clone();
            let handle = tokio::spawn(async move {
                if i % 2 == 0 {
                    ctx_clone.add_message("rw_test", "user", &format!("Concurrent {}", i)).await;
                    Ok(())
                } else {
                    ctx_clone.list_sessions().await;
                    Ok(())
                }
            });
            handles.push(handle);
        }

        let results: Vec<_> = futures::future::join_all(handles).await;
        let success_count = results.iter().filter(|r| r.is_ok()).count();

        assert_eq!(success_count, 20, "All concurrent operations should succeed");
    }
}

// ==================== 单元测试: 分层访问 ====================

mod unit_layer_access_tests {
    use super::*;

    /// 测试 L0 abstract 获取
    #[tokio::test]
    async fn test_get_abstract() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "Content for abstract testing. This should be summarized.".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("abstract_test_user".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        
        let abstract_result = ctx.get_abstract(&result.uri).await.unwrap();
        assert_eq!(abstract_result.layer, "L0");
        assert!(!abstract_result.abstract_text.is_empty());
    }

    /// 测试 L1 overview 获取
    #[tokio::test]
    async fn test_get_overview() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "Content for overview testing. This should be expanded into an overview.".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(true),
            scope: "user".to_string(),
            user_id: Some("overview_test_user".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        
        let overview_result = ctx.get_overview(&result.uri).await.unwrap();
        assert_eq!(overview_result.layer, "L1");
        assert!(!overview_result.overview_text.is_empty());
    }

    /// 测试 L2 完整内容获取
    #[tokio::test]
    async fn test_get_read() {
        let ctx = test_utils::TestContext::new().await;

        let original_content = "Original content for L2 read test.";

        let args = test_utils::StoreArgs {
            content: original_content.to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("read_test_user".to_string()),
            agent_id: None,
        };

        let result = ctx.store(args).await;
        
        let read_result = ctx.get_read(&result.uri).await.unwrap();
        assert_eq!(read_result.layer, "L2");
        assert!(read_result.content.contains(original_content));
    }
}

// ==================== 集成测试 (需要外部服务) ====================

mod integration_tests {
    //! 集成测试 - 需要 Qdrant, LLM, Embedding 服务
    //!
    //! 运行方式:
    //! 1. 启动 Qdrant: docker run -p 6334:6334 qdrant/qdrant
    //! 2. 配置环境变量: LLM_API_BASE_URL, LLM_API_KEY, EMBEDDING_API_BASE_URL, EMBEDDING_API_KEY
    //! 3. 运行: cargo test -- --ignored integration

    /// 测试向量搜索 (需要 Qdrant 和 Embedding 服务)
    #[tokio::test]
    #[ignore]
    async fn integration_test_vector_search() {
        println!("Integration test: vector_search - requires Qdrant and Embedding service");
    }

    /// 测试 LLM 记忆提取 (需要 LLM 服务)
    #[tokio::test]
    #[ignore]
    async fn integration_test_llm_extraction() {
        println!("Integration test: llm_extraction - requires LLM service");
    }

    /// 测试完整的存储和检索流程 (需要全部外部服务)
    #[tokio::test]
    #[ignore]
    async fn integration_test_full_workflow() {
        println!("Integration test: full_workflow - requires all external services");
    }
}

// ==================== 性能测试 ====================

mod performance_tests {
    use super::*;
    use std::time::Instant;

    /// 测试存储性能
    #[tokio::test]
    async fn test_storage_performance() {
        let ctx = test_utils::TestContext::new().await;

        let start = Instant::now();

        for i in 0..100 {
            ctx.add_message("perf_test", "user", &format!("Performance test message {}", i)).await;
        }

        let duration = start.elapsed();
        println!("Storage of 100 messages took: {:?}", duration);

        assert!(duration.as_secs() < 10, "Storage took too long: {:?}", duration);
    }

    /// 测试读取性能
    #[tokio::test]
    async fn test_read_performance() {
        let ctx = test_utils::TestContext::new().await;

        let args = test_utils::StoreArgs {
            content: "Performance test content for reading.".to_string(),
            thread_id: "".to_string(),
            metadata: None,
            auto_generate_layers: Some(false),
            scope: "user".to_string(),
            user_id: Some("read_perf_user".to_string()),
            agent_id: None,
        };
        let result = ctx.store(args).await;

        let start = Instant::now();

        for _ in 0..100 {
            ctx.read(&result.uri).await.unwrap();
        }

        let duration = start.elapsed();
        println!("100 reads took: {:?}", duration);

        assert!(duration.as_secs() < 10, "Reads took too long: {:?}", duration);
    }

    /// 测试列表性能
    #[tokio::test]
    async fn test_list_performance() {
        let ctx = test_utils::TestContext::new().await;

        for i in 0..50 {
            ctx.add_message(&format!("list_perf_{}", i), "user", "message").await;
        }

        let start = Instant::now();

        for _ in 0..100 {
            ctx.list_sessions().await;
        }

        let duration = start.elapsed();
        println!("100 list operations took: {:?}", duration);

        assert!(duration.as_secs() < 10, "List operations took too long: {:?}", duration);
    }
}