//! # Cortex-Mem Core Library
//!
//! Cortex-Mem 是一个基于文件系统的记忆管理系统，支持向量搜索、会话管理和智能记忆提取。
//!
//! ## 主要功能
//!
//! - **文件系统**: 基于 `cortex://` URI 的虚拟文件系统
//! - **向量搜索**: 集成 Qdrant 向量数据库，支持语义搜索
//! - **会话管理**: 多线程会话管理，支持时间轴和参与者
//! - **记忆提取**: 使用 LLM 自动提取和分类记忆
//! - **索引自动化**: 自动监听文件变化并增量索引
//! - **增量更新**: 支持记忆的版本追踪、增量更新和层级联动
//!
//! ## 快速开始
//!
//! ```rust,no_run
//! use cortex_mem_core::{CortexFilesystem, FilesystemOperations};
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 初始化文件系统
//!     let filesystem = Arc::new(CortexFilesystem::new("./cortex-data"));
//!     filesystem.initialize().await?;
//!
//!     // 写入数据
//!     filesystem.write("cortex://test.md", "Hello, Cortex!").await?;
//!
//!     // 读取数据
//!     let content = filesystem.read("cortex://test.md").await?;
//!     println!("Content: {}", content);
//!
//!     Ok(())
//! }
//! ```
//!
//! ## 模块说明
//!
//! - [`filesystem`]: 文件系统操作和 URI 处理
//! - [`session`]: 会话管理和消息处理
//! - [`vector_store`]: 向量存储接口
//! - [`embedding`]: Embedding 生成客户端
//! - [`search`]: 向量搜索引擎
//! - [`automation`]: 自动化索引和提取
//! - [`extraction`]: 记忆提取和分类
//! - [`llm`]: LLM 客户端接口
//! - [`memory_index`]: 记忆索引和版本追踪
//! - [`memory_events`]: 记忆事件系统
//! - [`memory_index_manager`]: 记忆索引管理器
//! - [`incremental_memory_updater`]: 增量记忆更新器
//! - [`cascade_layer_updater`]: 层级联动更新器
//! - [`vector_sync_manager`]: 向量同步管理器
//! - [`memory_event_coordinator`]: 记忆事件协调器

pub mod config;
pub mod error;
pub mod events;
pub mod logging;
pub mod types;

pub mod automation;
pub mod builder;
pub mod embedding;
pub mod extraction;
pub mod filesystem;
pub mod layers;
pub mod llm;
pub mod search;
pub mod session;
pub mod vector_store;

// New modules for v2.5 incremental update system
pub mod memory_index;
pub mod memory_events;
pub mod memory_index_manager;
pub mod incremental_memory_updater;
pub mod cascade_layer_updater;
pub mod cascade_layer_debouncer;  // Phase 2 optimization
pub mod llm_result_cache;          // Phase 3 optimization (LLM cache only)
pub mod vector_sync_manager;
pub mod memory_event_coordinator;

// Re-exports
pub use config::*;
pub use error::{Error, Result};
pub use events::{CortexEvent, EventBus, FilesystemEvent, SessionEvent};
// Note: types::* exports V1MemoryType (and deprecated MemoryType alias for backward compatibility)
pub use types::*;

pub use automation::{
    AutoExtractConfig, AutoExtractor, AutoIndexer, AutomationConfig, AutomationManager, FsWatcher,
    IndexStats, IndexerConfig, SyncConfig, SyncManager, SyncStats, WatcherConfig,
};
pub use builder::{CortexMem, CortexMemBuilder};
pub use extraction::ExtractionConfig;
// Note: MemoryExtractor is also exported from session module
pub use embedding::{EmbeddingClient, EmbeddingConfig};
pub use filesystem::{CortexFilesystem, FilesystemOperations};
pub use llm::LLMClient;
pub use search::{SearchOptions, VectorSearchEngine};
pub use session::{
    CaseMemory, EntityMemory, EventMemory, ExtractedMemories, MemoryExtractor, Message,
    MessageRole, Participant, ParticipantManager, PreferenceMemory, SessionConfig, SessionManager,
};
pub use vector_store::{QdrantVectorStore, VectorStore, parse_vector_id, uri_to_vector_id};

// New re-exports for v2.5
// MemoryType from memory_index is the primary type for v2.5
pub use memory_index::{
    MemoryIndex, MemoryMetadata, MemoryScope, MemoryType, MemoryUpdateResult,
    SessionExtractionSummary,
};
pub use memory_events::{
    ChangeType, DeleteReason, EventStats, MemoryEvent,
};
pub use memory_index_manager::MemoryIndexManager;
pub use incremental_memory_updater::IncrementalMemoryUpdater;
pub use cascade_layer_updater::{CascadeLayerUpdater, UpdateStats};
pub use cascade_layer_debouncer::{LayerUpdateDebouncer, DebouncerConfig};  // Phase 2
pub use llm_result_cache::{LlmResultCache, CacheConfig, CacheStats};      // Phase 3
pub use vector_sync_manager::{VectorSyncManager, VectorSyncStats};
pub use memory_event_coordinator::{MemoryEventCoordinator, CoordinatorConfig};  // Phase 2

// Session-related re-exports
pub use session::message::MessageStorage;
