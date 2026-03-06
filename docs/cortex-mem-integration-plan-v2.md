# Cortex-Memory 集成方案 (Crate 直接集成)

## 概述

本文档描述如何将 **cortex-mem** 作为 zeroclaw 的 memory backend 进行集成。采用 **Crate 直接依赖**方式,充分利用 zeroclaw 的现有配置,实现零配置负担的高性能内存管理。

## 一、核心优势

### 1.1 零配置负担

✅ **智能配置复用** - 自动从 zeroclaw 配置推导所有必需参数
✅ **零性能损耗** - 直接调用 cortex-mem-tools API
✅ **类型安全** - Rust 编译期保证接口正确性
✅ **调试友好** - 单进程内调试,无需跨进程 tracing

### 1.2 用户只需两步配置

```toml
# config.toml

# 1. Zeroclaw 现有配置 (无需修改)
api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
# 2. 切换到 cortex backend
backend = "cortex"
embedding_provider = "openai"
embedding_model = "text-embedding-3-small"

# 3. 唯一必需的新配置
[memory.cortex]
qdrant_url = "http://localhost:6334"
```

**自动推导:**
- LLM API → 从 `api_key` + `default_provider` 推导
- LLM Model → 使用 `default_model`
- Embedding API → 从 `embedding_provider` 推导
- Embedding Model → 使用 `embedding_model`

## 二、技术实现

### 2.1 依赖配置

```toml
# Cargo.toml

[dependencies]
cortex-mem-tools = "0.3"  # 高层 API
cortex-mem-core = "0.3"   # 底层引擎
cortex-mem-config = "0.3" # 配置解析
```

### 2.2 配置结构

```rust
// src/config/schema.rs

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CortexMemConfig {
    /// Cortex 数据存储目录
    #[serde(default)]
    pub data_dir: Option<String>,
    
    /// Qdrant URL (必需)
    pub qdrant_url: Option<String>,
    
    /// Qdrant collection 名称
    #[serde(default)]
    pub qdrant_collection: Option<String>,
    
    /// Qdrant API Key (可选)
    #[serde(default)]
    pub qdrant_api_key: Option<String>,
    
    /// 租户标识
    #[serde(default = "default_cortex_tenant")]
    pub tenant_id: String,
    
    /// 覆盖 LLM model (用于记忆提取)
    #[serde(default)]
    pub llm_model_override: Option<String>,
    
    /// 覆盖 LLM temperature
    #[serde(default)]
    pub llm_temperature: Option<f32>,
    
    /// 覆盖 Embedding API Key
    #[serde(default)]
    pub embedding_api_key_override: Option<String>,
    
    /// 自动索引
    #[serde(default = "default_true")]
    pub auto_index: bool,
    
    /// 自动提取
    #[serde(default = "default_true")]
    pub auto_extract: bool,
}

fn default_cortex_tenant() -> String { "zeroclaw".into() }
fn default_true() -> bool { true }
```

### 2.3 智能配置推导

```rust
// src/memory/cortex_config_resolver.rs

use crate::config::{Config, MemoryConfig};
use cortex_mem_core::{
    llm::LLMConfig,
    embedding::EmbeddingConfig,
    config::QdrantConfig,
};

/// 从 Zeroclaw 配置自动推导 Cortex-Memory 配置
pub fn resolve_cortex_config(
    zeroclaw_config: &Config,
    memory_config: &MemoryConfig,
) -> anyhow::Result<(LLMConfig, EmbeddingConfig, QdrantConfig)> {
    // 1. 解析 LLM 配置 (自动复用 zeroclaw 的 provider)
    let llm_config = resolve_llm_config(zeroclaw_config, &memory_config.cortex)?;
    
    // 2. 解析 Embedding 配置 (支持 hint: 路由)
    let embedding_config = resolve_embedding_config(zeroclaw_config, memory_config)?;
    
    // 3. 解析 Qdrant 配置
    let qdrant_config = resolve_qdrant_config(&memory_config.cortex)?;
    
    Ok((llm_config, embedding_config, qdrant_config))
}

/// 解析 LLM 配置 - 智能复用 zeroclaw 的 default_provider
fn resolve_llm_config(
    zeroclaw_config: &Config,
    cortex_config: &CortexMemConfig,
) -> anyhow::Result<LLMConfig> {
    // 优先级 1: Cortex 专属覆盖
    if let Some(ref model_override) = cortex_config.llm_model_override {
        return Ok(LLMConfig {
            api_base_url: zeroclaw_config.api_url.clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            api_key: zeroclaw_config.api_key.clone()
                .ok_or_else(|| anyhow::anyhow!("api_key is required"))?,
            model_efficient: model_override.clone(),
            temperature: cortex_config.llm_temperature.unwrap_or(0.3),
            max_tokens: 4096,
        });
    }
    
    // 优先级 2: 从 default_provider 自动推导
    let api_base_url = zeroclaw_config.api_url.clone()
        .unwrap_or_else(|| {
            match zeroclaw_config.default_provider.as_deref() {
                Some("openai") => "https://api.openai.com/v1".to_string(),
                Some("anthropic") => "https://api.anthropic.com/v1".to_string(),
                Some("ollama") => "http://localhost:11434/v1".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            }
        });
    
    Ok(LLMConfig {
        api_base_url,
        api_key: zeroclaw_config.api_key.clone()
            .ok_or_else(|| anyhow::anyhow!("api_key is required"))?,
        model_efficient: zeroclaw_config.default_model.clone()
            .unwrap_or_else(|| "gpt-3.5-turbo".to_string()),
        temperature: zeroclaw_config.default_temperature as f32,
        max_tokens: 4096,
    })
}

/// 解析 Embedding 配置 - 支持 hint: 路由机制
fn resolve_embedding_config(
    zeroclaw_config: &Config,
    memory_config: &MemoryConfig,
) -> anyhow::Result<EmbeddingConfig> {
    // 检查是否使用 hint 路由
    if let Some(hint) = memory_config.embedding_model.strip_prefix("hint:") {
        // 查找匹配的 embedding_routes
        let route = zeroclaw_config.embedding_routes
            .iter()
            .find(|r| r.hint == hint)
            .ok_or_else(|| anyhow::anyhow!("No matching embedding route for hint: {}", hint))?;
        
        return Ok(EmbeddingConfig {
            api_base_url: provider_to_base_url(&route.provider),
            api_key: route.api_key.clone()
                .or_else(|| zeroclaw_config.api_key.clone())
                .ok_or_else(|| anyhow::anyhow!("API key required for embedding"))?,
            model_name: route.model.clone(),
            batch_size: 10,
            timeout_secs: 30,
        });
    }
    
    // 直接使用 memory 配置
    let api_key = memory_config.cortex.embedding_api_key_override.clone()
        .or_else(|| zeroclaw_config.api_key.clone())
        .ok_or_else(|| anyhow::anyhow!("API key required for embedding"))?;
    
    Ok(EmbeddingConfig {
        api_base_url: provider_to_base_url(&memory_config.embedding_provider),
        api_key,
        model_name: memory_config.embedding_model.clone(),
        batch_size: 10,
        timeout_secs: 30,
    })
}

/// Provider 名称转 Base URL
fn provider_to_base_url(provider: &str) -> String {
    if provider.starts_with("custom:") {
        return provider.strip_prefix("custom:").unwrap().to_string();
    }
    
    match provider {
        "openai" => "https://api.openai.com/v1".to_string(),
        _ => "https://api.openai.com/v1".to_string(),
    }
}

/// 解析 Qdrant 配置
fn resolve_qdrant_config(cortex_config: &CortexMemConfig) -> anyhow::Result<QdrantConfig> {
    Ok(QdrantConfig {
        url: cortex_config.qdrant_url.clone()
            .unwrap_or_else(|| "http://localhost:6334".to_string()),
        collection_name: cortex_config.qdrant_collection.clone()
            .unwrap_or_else(|| "zeroclaw-memory".to_string()),
        embedding_dim: Some(1536),
        timeout_secs: 30,
        api_key: cortex_config.qdrant_api_key.clone(),
        tenant_id: Some(cortex_config.tenant_id.clone()),
    })
}
```

### 2.4 CortexMemory 实现

```rust
// src/memory/cortex.rs

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use super::cortex_config_resolver::resolve_cortex_config;
use crate::config::{Config, MemoryConfig};
use anyhow::Result;
use async_trait::async_trait;
use cortex_mem_tools::MemoryOperations;
use cortex_mem_core::llm::LLMClientImpl;
use std::path::PathBuf;
use std::sync::Arc;

/// Cortex-Memory backend (Crate 直接集成)
pub struct CortexMemory {
    operations: Arc<MemoryOperations>,
    workspace_dir: PathBuf,
}

impl CortexMemory {
    pub async fn new(
        memory_config: &MemoryConfig,
        workspace_dir: PathBuf,
        zeroclaw_config: &Config,
    ) -> Result<Self> {
        // 自动推导配置
        let (llm_config, embedding_config, qdrant_config) = 
            resolve_cortex_config(zeroclaw_config, memory_config)?;
        
        let cortex_config = &memory_config.cortex;
        let data_dir = cortex_config.data_dir.clone()
            .unwrap_or_else(|| workspace_dir.join("cortex-data").to_string_lossy().to_string());
        
        tracing::info!(
            "🧠 Cortex-Memory initialized:\n\
             ├─ LLM: {} @ {}\n\
             ├─ Embedding: {} ({} dims)\n\
             └─ Qdrant: {} / {}",
            llm_config.model_efficient,
            llm_config.api_base_url,
            embedding_config.model_name,
            qdrant_config.embedding_dim.unwrap_or(1536),
            qdrant_config.url,
            qdrant_config.collection_name,
        );
        
        // 创建 LLM Client
        let llm_client = Arc::new(LLMClientImpl::new(llm_config)?);
        
        // 初始化 MemoryOperations
        let operations = MemoryOperations::new(
            &data_dir,
            &cortex_config.tenant_id,
            llm_client,
            &qdrant_config.url,
            &qdrant_config.collection_name,
            qdrant_config.api_key.as_deref(),
            &embedding_config.api_base_url,
            &embedding_config.api_key,
            &embedding_config.model_name,
            qdrant_config.embedding_dim,
            None,
        ).await?;
        
        Ok(Self {
            operations: Arc::new(operations),
            workspace_dir,
        })
    }
}

#[async_trait]
impl Memory for CortexMemory {
    fn name(&self) -> &str {
        "cortex"
    }
    
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let thread_id = session_id.unwrap_or("default");
        
        // 使用 MemoryOperations API
        self.operations.add_message(thread_id, "user", content).await?;
        
        tracing::debug!("Cortex stored message in thread {}", thread_id);
        Ok(())
    }
    
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        use cortex_mem_core::SearchOptions;
        
        // 构建搜索选项
        let root_uri = session_id.map(|sid| format!("cortex://session/{}", sid));
        
        let options = SearchOptions {
            limit,
            threshold: 0.4,
            root_uri,
            recursive: true,
        };
        
        // 执行语义搜索
        let results = self.operations
            .vector_engine()
            .layered_semantic_search(query, &options)
            .await?;
        
        // 转换为 MemoryEntry
        let entries: Vec<MemoryEntry> = results
            .into_iter()
            .map(|r| MemoryEntry {
                id: r.uri.clone(),
                key: r.uri,
                content: r.content.unwrap_or_default(),
                category: MemoryCategory::Conversation,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: session_id.map(str::to_string),
                score: Some(r.score as f64),
            })
            .collect();
        
        Ok(entries)
    }
    
    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        match self.operations.read_file(key).await {
            Ok(content) => Ok(Some(MemoryEntry {
                id: key.to_string(),
                key: key.to_string(),
                content,
                category: MemoryCategory::Conversation,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: None,
                score: None,
            })),
            Err(_) => Ok(None),
        }
    }
    
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let scope_uri = match category {
            Some(MemoryCategory::Core) => "cortex://user",
            _ => session_id
                .map(|sid| format!("cortex://session/{}", sid))
                .unwrap_or_else(|| "cortex://session".to_string()),
        };
        
        let entries = self.operations.filesystem().list(&scope_uri).await?;
        
        // 转换为 MemoryEntry
        let memory_entries: Vec<MemoryEntry> = entries
            .into_iter()
            .filter(|e| !e.is_directory)
            .map(|e| MemoryEntry {
                id: e.uri.clone(),
                key: e.uri,
                content: String::new(),
                category: category.cloned().unwrap_or(MemoryCategory::Conversation),
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: session_id.map(str::to_string),
                score: None,
            })
            .collect();
        
        Ok(memory_entries)
    }
    
    async fn forget(&self, key: &str) -> Result<bool> {
        match self.operations.delete(key).await {
            Ok(_) => Ok(true),
            Err(e) => {
                tracing::debug!("Cortex forget failed: {}", e);
                Ok(false)
            }
        }
    }
    
    async fn count(&self) -> Result<usize> {
        let sessions = self.operations.list_sessions().await?;
        Ok(sessions.len())
    }
    
    async fn health_check(&self) -> bool {
        self.operations.vector_engine()
            .semantic_search("test", &cortex_mem_core::SearchOptions::default())
            .await
            .is_ok()
    }
}
```

### 2.5 Factory 注册

```rust
// src/memory/backend.rs

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MemoryBackendKind {
    Sqlite,
    Lucid,
    Postgres,
    Mariadb,
    Qdrant,
    Markdown,
    Cortex,  // 新增
    None,
    Unknown,
}

pub fn classify_memory_backend(backend: &str) -> MemoryBackendKind {
    match backend {
        "sqlite" => MemoryBackendKind::Sqlite,
        "lucid" => MemoryBackendKind::Lucid,
        "postgres" => MemoryBackendKind::Postgres,
        "mariadb" | "mysql" => MemoryBackendKind::Mariadb,
        "qdrant" => MemoryBackendKind::Qdrant,
        "markdown" => MemoryBackendKind::Markdown,
        "cortex" => MemoryBackendKind::Cortex,  // 新增
        "none" => MemoryBackendKind::None,
        _ => MemoryBackendKind::Unknown,
    }
}
```

```rust
// src/memory/mod.rs

pub fn create_memory_with_storage_and_routes(
    config: &MemoryConfig,
    embedding_routes: &[EmbeddingRouteConfig],
    storage_provider: Option<&StorageProviderConfig>,
    workspace_dir: &Path,
    api_key: Option<&str>,
) -> anyhow::Result<Box<dyn Memory>> {
    let backend_name = effective_memory_backend_name(&config.backend, storage_provider);
    let backend_kind = classify_memory_backend(&backend_name);
    
    // ... 现有代码 ...
    
    // 新增 Cortex 分支
    if matches!(backend_kind, MemoryBackendKind::Cortex) {
        let zeroclaw_config = crate::config::Config::load_from_workspace(workspace_dir)?;
        return Ok(Box::new(
            CortexMemory::new(config, workspace_dir.to_path_buf(), &zeroclaw_config).await?
        ));
    }
    
    // ... 其他 backend ...
}
```

## 三、实现步骤

### 阶段一: 基础架构 (2-3天)

1. **配置结构**
   - 在 `src/config/schema.rs` 中添加 `CortexMemConfig`
   - 在 `MemoryConfig` 中添加 `cortex` 字段

2. **Backend 注册**
   - 在 `src/memory/backend.rs` 添加 `MemoryBackendKind::Cortex`
   - 更新 `classify_memory_backend()` 函数

3. **依赖配置**
   - 在 `Cargo.toml` 添加 cortex-mem 依赖

### 阶段二: 核心实现 (2-3天)

1. **配置解析器**
   - 实现 `cortex_config_resolver.rs`
   - 实现智能配置推导逻辑

2. **CortexMemory**
   - 实现 `Memory` trait
   - 集成 `MemoryOperations` API

3. **Factory 集成**
   - 在 `create_memory()` 中添加 Cortex 分支

### 阶段三: 测试与文档 (1-2天)

1. **单元测试**
   - 测试配置推导逻辑
   - 测试 Memory trait 实现

2. **集成测试**
   - 测试与真实 Qdrant 的交互
   - 测试会话管理

3. **文档更新**
   - 更新 `docs/config-reference.md`
   - 添加使用示例

## 四、配置示例

### 4.1 最小配置 (推荐)

```toml
api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "cortex"
embedding_provider = "openai"
embedding_model = "text-embedding-3-small"

[memory.cortex]
qdrant_url = "http://localhost:6334"
```

### 4.2 使用 Embedding 路由

```toml
[memory]
backend = "cortex"
embedding_model = "hint:semantic"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-large"
dimensions = 3072

[memory.cortex]
qdrant_url = "http://localhost:6334"
```

### 4.3 自定义 Provider

```toml
api_key = "${OPENAI_API_KEY}"
default_provider = "ollama"
default_model = "llama3"

[memory]
backend = "cortex"
embedding_provider = "custom:http://localhost:8080/v1"
embedding_model = "custom-embed-v1"
embedding_dimensions = 768

[memory.cortex]
qdrant_url = "http://localhost:6334"
```

## 五、总结

通过 **智能配置复用** 机制,cortex-mem 集成对用户来说是 **零额外配置负担**:

✅ **自动推导** - LLM 和 Embedding 配置自动从 zeroclaw 继承
✅ **灵活覆盖** - 支持通过 cortex 专属配置覆盖任何参数
✅ **路由支持** - 完全支持 zeroclaw 的 `embedding_routes` 机制
✅ **类型安全** - Crate 直接集成,编译期保证正确性
✅ **高性能** - 零进程通信开销

**用户只需两步:**
1. 设置 `memory.backend = "cortex"`
2. 配置 Qdrant URL

其他配置全部自动继承!
