# Cortex-Memory 集成方案 (基于 Crate 直接集成)

## 概述

本文档详细描述了如何将 cortex-mem 作为 zeroclaw 的 memory backend 进行有效集成。cortex-mem 是一个基于 Rust 的高性能 AI 内存框架,提供三层记忆架构(L0/L1/L2)、向量语义搜索和智能内存提取能力。

**核心优势:** 采用 **Crate 直接集成** 方式,zeroclaw 作为纯 Rust 项目,直接依赖 `cortex-mem-tools` 和 `cortex-mem-core`,实现高性能的内存管理。

## 一、问题分析

### 1.1 Zeroclaw 现有 Memory Backend 架构

Zeroclaw 采用 trait-based 的可扩展架构:

```rust
// src/memory/traits.rs
#[async_trait]
pub trait Memory: Send + Sync {
    fn name(&self) -> &str;
    
    // 核心操作
    async fn store(&self, key: &str, content: &str, category: MemoryCategory, session_id: Option<&str>) -> anyhow::Result<()>;
    async fn recall(&self, query: &str, limit: usize, session_id: Option<&str>) -> anyhow::Result<Vec<MemoryEntry>>;
    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>>;
    async fn list(&self, category: Option<&MemoryCategory>, session_id: Option<&str>) -> anyhow::Result<Vec<MemoryEntry>>;
    async fn forget(&self, key: &str) -> anyhow::Result<bool>;
    async fn count(&self) -> anyhow::Result<usize>;
    async fn health_check(&self) -> bool;
}
```

**现有后端实现:**
- `sqlite` - SQLite + 向量搜索(推荐)
- `lucid` - Lucid Memory 桥接 + SQLite fallback
- `markdown` - Markdown 文件存储
- `postgres` / `mariadb` - SQL 数据库
- `qdrant` - Qdrant 向量数据库
- `none` - 无存储

**Factory 模式:**
- `src/memory/mod.rs` 中的 `create_memory()` 根据配置创建对应后端
- `src/memory/backend.rs` 定义了 `MemoryBackendKind` 枚举和 `MemoryBackendProfile`

### 1.2 Cortex-Mem 核心能力

**三层记忆架构:**
- **L0 (Abstract)**: ~100 tokens, 粗粒度候选选择 (权重 20%)
- **L1 (Overview)**: ~500-2000 tokens, 结构化摘要 (权重 30%)
- **L2 (Detail)**: 完整对话内容 (权重 50%)

**虚拟文件系统:**
```
cortex://session/{session_id}/timeline/{date}/{time}.md
cortex://user/preferences/{name}.md
cortex://agent/cases/{case_id}.md
```

**核心组件 (Crate 形式):**
- `cortex-mem-core`: 核心引擎 - 文件系统抽象、LLM客户端、Embedding、向量搜索、会话管理、内存提取
- `cortex-mem-tools`: MemoryOperations API - 高级封装,提供简洁的接口
- `cortex-mem-config`: 配置管理 - TOML配置加载和环境变量解析

**MemoryOperations API:**
```rust
// 高级 API 封装
pub struct MemoryOperations {
    pub async fn add_message(&self, thread_id: &str, role: &str, content: &str) -> Result<String>;
    pub async fn close_session(&self, thread_id: &str) -> Result<()>;
    pub fn vector_engine(&self) -> &Arc<VectorSearchEngine>;
    pub fn filesystem(&self) -> &Arc<CortexFilesystem>;
    pub async fn ensure_all_layers(&self) -> Result<GenerationStats>;
    pub async fn index_all_files(&self) -> Result<SyncStats>;
}
```

**VectorSearchEngine API:**
```rust
// 三层加权语义搜索
pub struct VectorSearchEngine {
    pub async fn layered_semantic_search(&self, query: &str, options: &SearchOptions) -> Result<Vec<SearchResult>>;
    pub async fn semantic_search(&self, query: &str, options: &SearchOptions) -> Result<Vec<SearchResult>>;
}
```

### 1.3 之前的错误实现

通过代码搜索发现,zeroclaw 代码库中**完全没有**cortex-mem 相关的集成代码。只有少量硬件相关的 "Cortex-M4" 引用(STM32 芯片),与 cortex-mem 框架无关。

**问题所在:** 之前的开发人员没有真正接入 cortex-mem,可能误解了需求或者采取了其他实现路径。

## 二、集成方案: Crate 直接集成

Zeroclaw 作为纯 Rust 项目,采用 **Crate 直接集成** 方式,依赖 `cortex-mem-tools` 和 `cortex-mem-core`,实现高性能的内存管理。

### 2.1 核心优势

1. ✅ **零性能损耗**: 直接调用 cortex-mem-tools API,无需进程间通信
2. ✅ **类型安全**: Rust 编译期保证接口正确性
3. ✅ **调试友好**: 单进程内调试,无需跨进程 tracing
4. ✅ **深度集成**: 可以直接访问 cortex-mem 的所有底层功能
5. ✅ **统一配置**: 在 zeroclaw 的 config.toml 中统一管理所有配置

### 2.2 新增 Memory Backend: `cortex`

**实现路径:**

```
src/memory/
├── cortex.rs           # 新增: CortexMemory 实现
├── cortex_config.rs    # 新增: CortexMemConfig 配置结构
├── mod.rs              # 修改: 导出 CortexMemory
└── backend.rs          # 修改: 添加 MemoryBackendKind::Cortex
```

### 2.3 依赖配置

```toml
# Cargo.toml

[dependencies]
# Cortex-Memory 核心依赖
cortex-mem-tools = "0.3"  # 高层 API 和工具
cortex-mem-core = "0.3"   # 底层引擎
cortex-mem-config = "0.3" # 配置解析
```

### 2.4 Zeroclaw 配置结构

**最小配置 (推荐):**

```toml
# config.toml

# === Zeroclaw 现有配置 (自动复用) ===
api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"
default_temperature = 0.7

[memory]
# 切换到 cortex backend
backend = "cortex"

# 现有的 embedding 配置 (自动复用)
embedding_provider = "openai"
embedding_model = "text-embedding-3-small"
embedding_dimensions = 1536

# === Cortex-Memory 专属配置 ===
[memory.cortex]
# 数据存储目录 (默认: workspace/cortex-data)
data_dir = "./data/cortex-memory"

# Qdrant 向量数据库配置 (必需)
qdrant_url = "http://localhost:6334"
qdrant_collection = "zeroclaw-memory"

# 租户标识,用于多租户隔离 (默认: "zeroclaw")
tenant_id = "zeroclaw"

# 自动化配置 (可选,默认全部启用)
auto_index = true
auto_extract = true
generate_layers_on_close = true
```

**完整配置 (可选覆盖):**

```toml
[memory.cortex]
# Cortex 专属配置
qdrant_url = "http://qdrant.example.com:6334"
qdrant_collection = "production-memory"
qdrant_api_key = "${QDRANT_API_KEY}"
tenant_id = "production-agent"
data_dir = "/var/lib/cortex"

# 覆盖 LLM 配置 (用于记忆提取)
llm_model_override = "gpt-4o-mini"  # 使用更便宜的模型
llm_temperature = 0.3  # 提取任务用更低温度

# 可选: 使用单独的 Embedding API Key
embedding_api_key_override = "${EMBEDDING_API_KEY}"

# 自动化配置
auto_index = true
auto_extract = true
generate_layers_on_close = true
```

**配置结构定义:**

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
    
    /// 会话关闭时生成 L0/L1
    #[serde(default = "default_true")]
    pub generate_layers_on_close: bool,
}

fn default_cortex_tenant() -> String {
    "zeroclaw".into()
}

fn default_true() -> bool {
    true
}

impl Default for CortexMemConfig {
    fn default() -> Self {
        Self {
            data_dir: None,
            qdrant_url: None,
            qdrant_collection: None,
            qdrant_api_key: None,
            tenant_id: default_cortex_tenant(),
            llm_model_override: None,
            llm_temperature: None,
            embedding_api_key_override: None,
            auto_index: true,
            auto_extract: true,
            generate_layers_on_close: true,
        }
    }
}

// 修改 MemoryConfig 结构
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryConfig {
    // ... 现有字段 ...
    
    /// Configuration for Cortex-Memory backend.
    /// Only used when `backend = "cortex"`.
    #[serde(default)]
    pub cortex: CortexMemConfig,
}
```

### 2.4 CortexMemory 实现

```rust
// src/memory/cortex.rs

use super::traits::{Memory, MemoryCategory, MemoryEntry};
use crate::config::CortexMemConfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Command;
use tokio::time::{timeout, Duration};

pub struct CortexMemory {
    config: CortexMemConfig,
    workspace_dir: PathBuf,
    config_path: PathBuf,
}

impl CortexMemory {
    pub fn new(config: CortexMemConfig, workspace_dir: &Path) -> Result<Self> {
        let config_path = config.config_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_dir.join("cortex-config.toml"));
        
        // 验证 cortex-mem-cli 是否可用
        Self::check_cli_available(&config.cli_path)?;
        
        Ok(Self {
            config,
            workspace_dir: workspace_dir.to_path_buf(),
            config_path,
        })
    }
    
    fn check_cli_available(cli_path: &str) -> Result<()> {
        let output = Command::new(cli_path)
            .arg("--version")
            .output()
            .context("Failed to execute cortex-mem-cli. Ensure it's installed and in PATH")?;
        
        if !output.status.success() {
            anyhow::bail!("cortex-mem-cli --version failed");
        }
        
        Ok(())
    }
    
    async fn execute_cli(&self, args: &[&str]) -> Result<String> {
        let duration = Duration::from_secs(self.config.timeout_secs);
        
        let output = timeout(duration, async {
            let mut cmd = Command::new(&self.config.cli_path);
            cmd.arg("--config")
               .arg(&self.config_path)
               .arg("--tenant")
               .arg(&self.config.tenant);
            
            if self.config.verbose {
                cmd.arg("--verbose");
            }
            
            cmd.args(args);
            
            let output = cmd.output()
                .context("Failed to execute cortex-mem-cli")?;
            
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("cortex-mem-cli failed: {}", stderr);
            }
            
            Ok::<_, anyhow::Error>(String::from_utf8(output.stdout)?)
        })
        .await
        .context("cortex-mem-cli execution timed out")??;
        
        Ok(output)
    }
    
    fn category_to_scope(category: &MemoryCategory) -> &'static str {
        match category {
            MemoryCategory::Core => "user",
            MemoryCategory::Daily => "session",
            MemoryCategory::Conversation => "session",
            MemoryCategory::Custom(_) => "session",
        }
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
        // 使用 session_id 作为 thread_id,如果没有则使用 key 作为标识
        let thread_id = session_id.unwrap_or(key);
        let scope = Self::category_to_scope(&category);
        
        // cortex-mem 的 add 命令会自动创建会话(如果不存在)
        self.execute_cli(&[
            "add",
            "--thread", thread_id,
            "--role", "user",
            content,
        ]).await?;
        
        Ok(())
    }
    
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let mut args = vec![
            "search",
            query,
            "--limit", &limit.to_string(),
        ];
        
        if let Some(session) = session_id {
            args.extend_from_slice(&["--thread", session]);
        }
        
        let output = self.execute_cli(&args).await?;
        
        // 解析 CLI 输出,提取内存条目
        // TODO: 根据实际 CLI 输出格式进行解析
        let entries = parse_search_output(&output)?;
        
        Ok(entries)
    }
    
    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        // 构造 cortex:// URI
        let uri = format!("cortex://session/{}/memory.md", key);
        
        match self.execute_cli(&["get", &uri]).await {
            Ok(content) => {
                let entry = MemoryEntry {
                    id: key.to_string(),
                    key: key.to_string(),
                    content,
                    category: MemoryCategory::Core,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    session_id: None,
                    score: None,
                };
                Ok(Some(entry))
            }
            Err(e) if e.to_string().contains("not found") => Ok(None),
            Err(e) => Err(e),
        }
    }
    
    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let uri = if let Some(session) = session_id {
            format!("cortex://session/{}", session)
        } else {
            match category {
                Some(MemoryCategory::Core) => "cortex://user".to_string(),
                _ => "cortex://session".to_string(),
            }
        };
        
        let output = self.execute_cli(&[
            "list",
            "--uri", &uri,
            "--include-abstracts",
        ]).await?;
        
        // 解析 CLI 输出
        let entries = parse_list_output(&output)?;
        
        Ok(entries)
    }
    
    async fn forget(&self, key: &str) -> Result<bool> {
        let uri = format!("cortex://session/{}/memory.md", key);
        
        match self.execute_cli(&["delete", &uri]).await {
            Ok(_) => Ok(true),
            Err(e) if e.to_string().contains("not found") => Ok(false),
            Err(e) => Err(e),
        }
    }
    
    async fn count(&self) -> Result<usize> {
        let output = self.execute_cli(&["stats"]).await?;
        
        // 解析统计输出
        // TODO: 根据实际 CLI 输出格式提取 count
        let count = parse_stats_count(&output)?;
        
        Ok(count)
    }
    
    async fn health_check(&self) -> bool {
        self.execute_cli(&["tenant", "list"]).await.is_ok()
    }
}

// 辅助解析函数
fn parse_search_output(output: &str) -> Result<Vec<MemoryEntry>> {
    // TODO: 实现实际的输出解析逻辑
    // 根据 cortex-mem-cli search 命令的实际输出格式进行解析
    Ok(vec![])
}

fn parse_list_output(output: &str) -> Result<Vec<MemoryEntry>> {
    // TODO: 实现实际的输出解析逻辑
    Ok(vec![])
}

fn parse_stats_count(output: &str) -> Result<usize> {
    // TODO: 实现实际的输出解析逻辑
    Ok(0)
}
```

### 2.5 Backend 枚举和工厂更新

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

const CORTEX_PROFILE: MemoryBackendProfile = MemoryBackendProfile {
    key: "cortex",
    label: "Cortex-Memory — AI-native hierarchical memory with semantic search",
    auto_save_default: true,
    uses_sqlite_hygiene: false,
    sqlite_based: false,
    optional_dependency: true,
};

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

pub fn memory_backend_profile(backend: &str) -> MemoryBackendProfile {
    match classify_memory_backend(backend) {
        // ... 现有匹配 ...
        MemoryBackendKind::Cortex => CORTEX_PROFILE,
        // ...
    }
}
```

```rust
// src/memory/mod.rs

pub mod cortex;  // 新增
pub mod cortex_config;  // 新增

pub use cortex::CortexMemory;  // 新增

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
    
    // 新增 Cortex backend 分支
    if matches!(backend_kind, MemoryBackendKind::Cortex) {
        let cortex_mem = CortexMemory::new(config.cortex.clone(), workspace_dir)?;
        return Ok(Box::new(cortex_mem));
    }
    
    // ... 现有代码 ...
}
```

## 五、数据模型映射

### 5.1 MemoryCategory 到 Cortex URI 的映射

| Zeroclaw Category | Cortex URI Pattern | 说明 |
|-------------------|-------------------|------|
| `Core` | `cortex://user/{tenant}/preferences/{key}.md` | 用户长期记忆、偏好 |
| `Daily` | `cortex://session/{session_id}/timeline/{date}.md` | 日常会话日志 |
| `Conversation` | `cortex://session/{session_id}/timeline/{datetime}.md` | 对话消息 |
| `Custom(name)` | `cortex://session/{session_id}/{name}/{key}.md` | 自定义分类 |

### 5.2 搜索策略映射

```rust
// Zeroclaw 的 recall() -> Cortex-Mem 的 layered_semantic_search()

// 1. 配置搜索选项
let options = SearchOptions {
    limit: 10,
    threshold: 0.4,  // 最小相关性
    root_uri: Some("cortex://session/{session_id}"),  // 限定范围
    recursive: true,
};

// 2. 执行三层加权搜索
// L0 (Abstract): 20% 权重, ~100 tokens, 快速候选筛选
// L1 (Overview): 30% 权重, ~500-2000 tokens, 结构化摘要
// L2 (Detail):   50% 权重, 完整内容, 精确匹配

let results = vector_engine.layered_semantic_search(query, &options).await?;
```

### 5.3 会话生命周期集成

```
Zeroclaw Agent Loop              Cortex-Memory Events
─────────────────────────────────────────────────────────────
1. 接收用户消息
   ↓
2. 调用 memory.store()
   → add_message(thread_id, role, content)
   → 触发 MessageAdded 事件
   → 自动索引 L2 层
   ↓
3. 执行 agent 逻辑
   ↓
4. 调用 memory.recall()
   → layered_semantic_search()
   → 返回三层加权结果
   ↓
5. 生成响应
   ↓
6. 会话结束 (可选)
   → close_session(thread_id)
   → 触发 SessionClosed 事件
   → 自动内存提取 (LLM)
   → 自动生成 L0/L1 层
   → 自动索引 L0/L1
```

## 六、实现步骤

### 阶段一: 基础框架 (2-3天)

**任务清单:**

1. **添加依赖**
   ```bash
   # 在 Cargo.toml 中添加
   cortex-mem-tools = "0.3"
   cortex-mem-core = "0.3"
   cortex-mem-config = "0.3"
   ```

2. **添加配置结构**
   - 在 `src/config/schema.rs` 中添加 `CortexMemConfig` 及相关配置
   - 在 `MemoryConfig` 中添加 `cortex` 字段
   - 实现默认值和验证逻辑

3. **创建 CortexMemory 实现**
   - 创建 `src/memory/cortex.rs`
   - 实现 `Memory` trait 的所有方法
   - 实现 `new()` 异步初始化函数

4. **更新 Backend 枚举**
   - 在 `src/memory/backend.rs` 中添加 `MemoryBackendKind::Cortex`
   - 添加 `CORTEX_PROFILE`
   - 更新 `classify_memory_backend()` 和 `memory_backend_profile()`

5. **更新 Factory 函数**
   - 在 `src/memory/mod.rs` 中导入 `CortexMemory`
   - 添加 `create_memory_async()` 异步工厂函数
   - 在工厂逻辑中添加 Cortex 分支

### 阶段二: LLM Client 适配 (1-2天)

**关键任务:**

1. **实现 LLMClient Adapter**
   ```rust
   // src/memory/cortex_llm_adapter.rs
   
   use cortex_mem_core::llm::{LLMClient, LLMConfig};
   use crate::providers::Provider;
   
   /// 将 Zeroclaw 的 Provider 适配为 Cortex-Mem 的 LLMClient
   pub struct ZeroclawLLMAdapter {
       provider: Arc<dyn Provider>,
       config: LLMConfig,
   }
   
   #[async_trait]
   impl LLMClient for ZeroclawLLMAdapter {
       async fn complete(&self, prompt: &str) -> Result<String> {
           // 调用 zeroclaw provider 的 complete 方法
           self.provider.complete(prompt).await
       }
       
       // ... 其他方法 ...
   }
   ```

2. **配置共享逻辑**
   - 从 `zeroclaw_config.default_provider` 提取 LLM 配置
   - 支持覆盖配置 (使用 `memory.cortex.llm`)

### 阶段三: 会话管理集成 (1天)

**任务:**

1. **在 Agent 中集成会话关闭**
   ```rust
   // src/agent/mod.rs
   
   impl Agent {
       pub async fn end_session(&mut self, session_id: &str) -> Result<()> {
           // 检查是否为 Cortex backend
           if let Some(cortex_mem) = self.memory.as_any().downcast_ref::<CortexMemory>() {
               cortex_mem.close_session(session_id).await?;
           }
           Ok(())
       }
   }
   ```

2. **自动触发机制**
   - 在会话超时或显式结束时自动调用
   - 添加配置开关控制是否自动提取

### 阶段四: 测试和文档 (2天)

**测试策略:**

1. **单元测试**
   ```rust
   #[cfg(test)]
   mod tests {
       use super::*;
       use tempfile::TempDir;
       
       #[tokio::test]
       async fn test_cortex_memory_store_and_recall() {
           let tmp = TempDir::new().unwrap();
           let config = create_test_config();
           
           let cortex = CortexMemory::new(config.cortex, tmp.path(), &config).await.unwrap();
           
           // 测试存储
           cortex.store("key1", "test content", MemoryCategory::Core, None).await.unwrap();
           
           // 测试搜索
           let results = cortex.recall("test", 10, None).await.unwrap();
           assert!(!results.is_empty());
       }
   }
   ```

2. **集成测试**
   - 需要 Qdrant 实例
   - 需要 LLM API (或 mock)
   - 测试完整的生命周期

3. **文档更新**
   - 更新 `docs/config-reference.md`
   - 添加 `docs/cortex-memory-guide.md`
   - 更新 `README.md`

## 七、配置示例

### 7.1 最小配置

```toml
# config.toml

[memory]
backend = "cortex"

[memory.cortex]
tenant_id = "my-agent"

[memory.cortex.qdrant]
url = "http://localhost:6334"
collection_name = "my-agent-memory"

[memory.cortex.embedding]
api_base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model_name = "text-embedding-3-small"
```

### 7.2 完整配置

```toml
# config.toml

[memory]
backend = "cortex"

[memory.cortex]
data_dir = "./data/cortex-memory"
tenant_id = "production-agent"
user_id = "agent-001"

[memory.cortex.qdrant]
url = "http://qdrant.example.com:6334"
collection_name = "zeroclaw-memory"
embedding_dim = 1536
timeout_secs = 30
api_key = "${QDRANT_API_KEY}"

[memory.cortex.embedding]
api_base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model_name = "text-embedding-3-small"
batch_size = 20
timeout_secs = 60

# 使用独立的 LLM 配置 (可选)
[memory.cortex.llm]
api_base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model_efficient = "gpt-4o-mini"
temperature = 0.7
max_tokens = 4096

[memory.cortex.automation]
auto_index = true
auto_extract = true
generate_layers_on_close = true
```

### 7.3 环境变量支持

```bash
# .env

# Cortex-Memory 配置
OPENAI_API_KEY=sk-...
QDRANT_API_KEY=your-qdrant-key
QDRANT_URL=http://localhost:6334

# 可选: 覆盖配置
CORTEX_DATA_DIR=/var/lib/cortex
CORTEX_TENANT_ID=production
```

## 八、性能优化

### 8.1 Embedding 缓存

```rust
// cortex-mem-core 已内置 LLM 结果缓存
// 配置缓存大小:
let cache_config = CacheConfig {
    max_entries: 1000,
    ttl_secs: 3600,  // 1小时
};
```

### 8.2 批量操作

```rust
// 批量存储消息
async fn store_batch(&self, messages: Vec<(String, String)>) -> Result<()> {
    for (role, content) in messages {
        self.operations.add_message(&thread_id, &role, &content).await?;
    }
    Ok(())
}
```

### 8.3 异步索引

```rust
// 自动化配置中启用异步索引
let automation_config = CortexAutomationConfig {
    auto_index: true,      // 消息添加时自动索引 L2
    auto_extract: true,    // 会话关闭时自动提取
    generate_layers_on_close: true,  // 自动生成 L0/L1
};
```

## 九、监控和调试

### 9.1 日志配置

```rust
// 初始化 cortex-mem 时启用详细日志
tracing_subscriber::fmt()
    .with_max_level(tracing::Level::DEBUG)
    .init();
```

### 9.2 性能指标

```rust
// 监控搜索性能
let start = std::time::Instant::now();
let results = cortex.recall(query, limit, session_id).await?;
let duration = start.elapsed();
tracing::info!("Search completed in {:?}", duration);
```

### 9.3 健康检查

```bash
# 检查 Qdrant 连接
curl http://localhost:6334/collections/zeroclaw-memory

# 检查 Cortex 数据目录
ls -la ./cortex-data/
```

## 十、迁移和兼容性

### 10.1 从其他 Backend 迁移

```bash
# 提供迁移命令
zeroclaw memory migrate --from sqlite --to cortex

# 实现:
# 1. 读取 sqlite 中的所有 memory
# 2. 为每个 memory 调用 cortex.store()
# 3. 验证迁移结果
```

### 10.2 后备机制

```rust
pub async fn create_memory_with_fallback(
    config: &MemoryConfig,
    workspace_dir: &Path,
    zeroclaw_config: &Config,
) -> anyhow::Result<Box<dyn Memory>> {
    if config.backend == "cortex" {
        match CortexMemory::new(config.cortex.clone(), workspace_dir, zeroclaw_config).await {
            Ok(mem) => return Ok(Box::new(mem)),
            Err(e) => {
                tracing::warn!("Failed to initialize cortex backend, falling back to sqlite: {}", e);
                // fallback to sqlite
            }
        }
    }
    // ... 其他 backend ...
}
```

## 十一、风险和缓解

### 11.1 依赖风险

**风险:** cortex-mem crate 版本不兼容

**缓解:**
- 使用 `Cargo.lock` 锁定版本
- 定期更新和测试
- 关注 cortex-mem 的 changelog

### 11.2 配置复杂度

**风险:** 配置项过多,用户难以配置

**缓解:**
- 提供合理的默认值
- 自动从 zeroclaw 配置推导
- 提供配置验证和错误提示

### 11.3 性能风险

**风险:** 大量消息时搜索性能下降

**缓解:**
- 利用三层架构减少 LLM context 开销
- 启用 embedding 缓存
- 配置合理的搜索阈值

## 十二、总结

本方案通过 **Crate 直接集成** 实现了 cortex-mem 与 zeroclaw 的深度融合:

**核心优势:**
1. ✅ 高性能: 无进程间通信开销
2. ✅ 类型安全: 编译期检查
3. ✅ 深度集成: 共享 LLM 和 Embedding
4. ✅ 三层记忆: L0/L1/L2 智能分层
5. ✅ 自动化: 消息索引、内存提取、层级生成全自动

**关键实现点:**
- 新增 `MemoryBackendKind::Cortex` 枚举值
- 实现 `CortexMemory` trait
- LLM Client 适配层
- 会话生命周期集成
- 配置统一管理

**开发估算:**
- 基础实现: 2-3 天
- LLM 适配: 1-2 天
- 会话集成: 1 天
- 测试文档: 2 天
- **总计: 6-8 天**

这个方案为 zeroclaw 提供了一个生产级的 cortex-mem 集成,既保持了架构的灵活性,又充分利用了 cortex-mem 的先进内存管理能力。通过三层记忆架构和语义搜索,zeroclaw 将具备更强的上下文理解和个性化能力。

## 十三、智能配置复用 (关键优势)

### 13.1 自动推导机制

**核心理念:** 用户无需重复配置 LLM 和 Embedding,自动从 zeroclaw 现有配置推导 cortex-mem 所需参数。

#### 13.1.1 配置映射表

| Cortex-Mem 参数 | Zeroclaw 来源 | 自动推导逻辑 |
|----------------|--------------|-------------|
| **LLM API Base URL** | `api_url` 或 provider base_url | 优先使用 `api_url`,否则从 `default_provider` 解析 |
| **LLM API Key** | `api_key` | 直接复用全局配置 |
| **LLM Model** | `default_model` | 用于记忆提取和层级生成 |
| **LLM Temperature** | `default_temperature` | 可选覆盖 |
| **Embedding Provider** | `memory.embedding_provider` | 支持 `openai` 或 `custom:URL` |
| **Embedding API Key** | `api_key` | 复用全局 key (或使用 embedding_routes 的 key) |
| **Embedding Model** | `memory.embedding_model` | 支持 `hint:` 路由机制 |
| **Embedding Dimensions** | `memory.embedding_dimensions` | 默认 1536 |

#### 13.1.2 配置优先级

```
1. Cortex 专属覆盖配置 (最高优先级)
   [memory.cortex.llm_model_override]
   
2. Zeroclaw 全局配置
   api_key, default_provider, default_model
   
3. Embedding 路由机制
   [[embedding_routes]] + memory.embedding_model = "hint:xxx"
   
4. 默认值 (最低优先级)
   http://localhost:6334 (Qdrant)
   text-embedding-3-small (Embedding model)
```

### 13.2 配置示例对比

#### 方案A: 最小配置 (推荐)

```toml
# config.toml

# === Zeroclaw 现有配置 ===
api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"
default_temperature = 0.7

[memory]
# 切换到 cortex backend
backend = "cortex"

# 现有的 embedding 配置 (自动复用)
embedding_provider = "openai"
embedding_model = "text-embedding-3-small"
embedding_dimensions = 1536

# === Cortex-Memory 专属配置 ===
[memory.cortex]
# 只需配置 Qdrant (必需)
qdrant_url = "http://localhost:6334"
qdrant_collection = "zeroclaw-memory"
```

**自动推导结果:**
- ✅ LLM: OpenAI API (使用全局 `api_key`)
- ✅ LLM Model: `gpt-4o-mini` (用于记忆提取)
- ✅ Embedding: OpenAI `text-embedding-3-small`
- ✅ Embedding Dimensions: 1536

#### 方案B: 使用 Embedding 路由

```toml
# config.toml

api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "cortex"

# 使用 hint 路由机制 (支持多 embedding provider)
embedding_provider = "openai"
embedding_model = "hint:semantic"  # 通过 hint 路由
embedding_dimensions = 1536

# Embedding 路由配置
[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-large"
dimensions = 3072

[memory.cortex]
qdrant_url = "http://localhost:6334"
qdrant_collection = "zeroclaw-memory"
```

**自动推导结果:**
- ✅ Embedding: 使用路由匹配的 `text-embedding-3-large` (3072维)
- ✅ 其他参数从全局配置继承

#### 方案C: 自定义 Embedding Provider

```toml
# config.toml

api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "cortex"

# 使用自定义 embedding 服务
embedding_provider = "custom:http://embedding-server:8080/v1"
embedding_model = "custom-embed-v1"
embedding_dimensions = 768

[memory.cortex]
qdrant_url = "http://localhost:6334"
qdrant_collection = "zeroclaw-memory"
```

**自动推导结果:**
- ✅ Embedding API: `http://embedding-server:8080/v1`
- ✅ Embedding Model: `custom-embed-v1` (768维)
- ✅ LLM: 仍然使用 OpenAI (全局配置)

#### 方案D: 完全覆盖配置

```toml
# config.toml

api_key = "${OPENAI_API_KEY}"
default_provider = "openai"
default_model = "gpt-4o-mini"

[memory]
backend = "cortex"
embedding_provider = "openai"
embedding_model = "text-embedding-3-small"
embedding_dimensions = 1536

[memory.cortex]
# Cortex 专属配置
qdrant_url = "http://qdrant.example.com:6334"
qdrant_collection = "production-memory"
qdrant_api_key = "${QDRANT_API_KEY}"
tenant_id = "production-agent"
data_dir = "/var/lib/cortex"

# 覆盖 LLM 配置 (用于记忆提取)
llm_model_override = "gpt-4o-mini"  # 使用更便宜的模型
llm_temperature = 0.3  # 提取任务用更低温度

# 可选: 使用单独的 Embedding API Key
embedding_api_key_override = "${EMBEDDING_API_KEY}"

# 自动化配置
auto_index = true
auto_extract = true
generate_layers_on_close = true
```

### 13.3 实现逻辑

#### 13.3.1 配置解析函数

```rust
// src/memory/cortex_config_resolver.rs

use crate::config::{Config, MemoryConfig, CortexMemConfig};
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
    // 1. 解析 LLM 配置
    let llm_config = resolve_llm_config(zeroclaw_config, &memory_config.cortex)?;
    
    // 2. 解析 Embedding 配置
    let embedding_config = resolve_embedding_config(zeroclaw_config, memory_config)?;
    
    // 3. 解析 Qdrant 配置
    let qdrant_config = resolve_qdrant_config(&memory_config.cortex)?;
    
    Ok((llm_config, embedding_config, qdrant_config))
}

/// 解析 LLM 配置 (智能复用 zeroclaw 的 provider)
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
                .ok_or_else(|| anyhow::anyhow!("api_key is required for LLM"))?,
            model_efficient: model_override.clone(),
            temperature: cortex_config.llm_temperature.unwrap_or(0.3),
            max_tokens: 4096,
        });
    }
    
    // 优先级 2: 从 default_provider 推导
    let api_base_url = zeroclaw_config.api_url.clone()
        .unwrap_or_else(|| {
            // 根据 default_provider 推导 base_url
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

/// 解析 Embedding 配置 (支持路由机制)
fn resolve_embedding_config(
    zeroclaw_config: &Config,
    memory_config: &MemoryConfig,
) -> anyhow::Result<EmbeddingConfig> {
    let cortex_config = &memory_config.cortex;
    
    // 检查是否使用 hint 路由
    let (provider, model, dimensions, api_key) = if let Some(hint) = 
        memory_config.embedding_model.strip_prefix("hint:") {
        
        // 查找匹配的 embedding_routes
        let route = zeroclaw_config.embedding_routes
            .iter()
            .find(|r| r.hint == hint)
            .ok_or_else(|| anyhow::anyhow!("No matching embedding route for hint: {}", hint))?;
        
        (
            route.provider.clone(),
            route.model.clone(),
            route.dimensions.unwrap_or(memory_config.embedding_dimensions),
            route.api_key.clone()
                .or_else(|| zeroclaw_config.api_key.clone())
                .ok_or_else(|| anyhow::anyhow!("API key required for embedding"))?,
        )
    } else {
        // 直接使用 memory 配置
        let provider = memory_config.embedding_provider.clone();
        let model = memory_config.embedding_model.clone();
        let dimensions = memory_config.embedding_dimensions;
        
        // 解析 provider 获取 base_url
        let api_base_url = if provider.starts_with("custom:") {
            provider.strip_prefix("custom:")
                .map(|s| s.to_string())
                .unwrap_or_else(|| "http://localhost:8080/v1".to_string())
        } else {
            // 使用 Cortex 专属覆盖或全局 key
            cortex_config.embedding_api_key_override.clone()
                .or_else(|| zeroclaw_config.api_key.clone())
                .ok_or_else(|| anyhow::anyhow!("API key required for embedding"))?;
            
            "https://api.openai.com/v1".to_string()
        };
        
        let api_key = cortex_config.embedding_api_key_override.clone()
            .or_else(|| zeroclaw_config.api_key.clone())
            .ok_or_else(|| anyhow::anyhow!("API key required for embedding"))?;
        
        (provider, model, dimensions, api_key)
    };
    
    Ok(EmbeddingConfig {
        api_base_url: provider_to_base_url(&provider),
        api_key,
        model_name: model,
        batch_size: 10,
        timeout_secs: 30,
    })
}

/// 解析 Qdrant 配置
fn resolve_qdrant_config(cortex_config: &CortexMemConfig) -> anyhow::Result<QdrantConfig> {
    Ok(QdrantConfig {
        url: cortex_config.qdrant_url.clone()
            .unwrap_or_else(|| "http://localhost:6334".to_string()),
        collection_name: cortex_config.qdrant_collection.clone()
            .unwrap_or_else(|| "zeroclaw-memory".to_string()),
        embedding_dim: Some(1536),  // 从 embedding 配置传入
        timeout_secs: 30,
        api_key: cortex_config.qdrant_api_key.clone(),
        tenant_id: Some(cortex_config.tenant_id.clone()
            .unwrap_or_else(|| "zeroclaw".to_string())),
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
```

#### 13.3.2 CortexMemory 初始化

```rust
// src/memory/cortex.rs

impl CortexMemory {
    pub async fn new(
        memory_config: &MemoryConfig,
        workspace_dir: &Path,
        zeroclaw_config: &Config,
    ) -> anyhow::Result<Self> {
        // 自动推导配置
        let (llm_config, embedding_config, qdrant_config) = 
            resolve_cortex_config(zeroclaw_config, memory_config)?;
        
        tracing::info!(
            "🧠 Initializing Cortex-Memory with auto-derived config:\n\
             LLM: {} @ {}\n\
             Embedding: {} ({} dims)\n\
             Qdrant: {} / {}",
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
            &tenant_id,
            llm_client,
            &qdrant_config.url,
            &qdrant_config.collection_name,
            qdrant_config.api_key.as_deref(),
            &embedding_config.api_base_url,
            &embedding_config.api_key,
            &embedding_config.model_name,
            qdrant_config.embedding_dim,
            cortex_config.user_id.clone(),
        ).await?;
        
        Ok(Self { operations, ... })
    }
}
```

### 13.4 配置验证

```rust
// src/config/validation.rs

/// 验证 Cortex-Memory 配置完整性
pub fn validate_cortex_config(config: &Config) -> anyhow::Result<()> {
    if config.memory.backend != "cortex" {
        return Ok(());
    }
    
    // 检查必需的配置
    if config.api_key.is_none() {
        anyhow::bail!(
            "Cortex-Memory requires api_key. \
             Set api_key in config.toml or OPENAI_API_KEY environment variable."
        );
    }
    
    // 检查 embedding 配置
    if config.memory.embedding_provider == "none" {
        anyhow::bail!(
            "Cortex-Memory requires a valid embedding_provider. \
             Set memory.embedding_provider = \"openai\" or \"custom:<url>\""
        );
    }
    
    // 检查 Qdrant 配置
    if config.memory.cortex.qdrant_url.is_none() {
        tracing::warn!(
            "Qdrant URL not specified, using default: http://localhost:6334. \
             Ensure Qdrant is running locally."
        );
    }
    
    Ok(())
}
```

### 13.5 用户友好提示

```rust
// 在 doctor 或启动时提供配置诊断

pub fn diagnose_cortex_config(config: &Config) {
    println!("🧠 Cortex-Memory Configuration:");
    println!("  LLM Provider: {}", config.default_provider.as_deref().unwrap_or("N/A"));
    println!("  LLM Model: {}", config.default_model.as_deref().unwrap_or("N/A"));
    println!("  Embedding: {} / {}", 
        config.memory.embedding_provider,
        config.memory.embedding_model
    );
    println!("  Qdrant: {}", 
        config.memory.cortex.qdrant_url.as_deref().unwrap_or("http://localhost:6334")
    );
    
    // 检测常见问题
    if config.memory.embedding_provider == "none" {
        println!("  ⚠️  Warning: embedding_provider is 'none', Cortex requires embeddings");
    }
    
    if config.api_key.is_none() {
        println!("  ⚠️  Warning: api_key not configured");
    }
}
```

## 十四、总结

通过智能配置复用机制,cortex-mem 集成对用户来说**零额外配置负担**:

✅ **自动推导:** LLM 和 Embedding 配置自动从 zeroclaw 继承
✅ **灵活覆盖:** 支持通过 cortex 专属配置覆盖任何参数
✅ **路由支持:** 完全支持 zeroclaw 的 `embedding_routes` 机制
✅ **配置验证:** 启动时自动检查配置完整性
✅ **用户友好:** 提供详细的配置诊断和错误提示

用户只需:
1. 将 `memory.backend` 改为 `"cortex"`
2. 确保 Qdrant 可访问
3. 其他配置自动继承!

## 三、实现步骤

### 阶段一: 基础框架 (1-2天)

1. **添加配置结构**
   - 在 `src/config/schema.rs` 中添加 `CortexMemConfig`
   - 在 `MemoryConfig` 中添加 `cortex` 字段
   - 添加默认值和验证逻辑

2. **创建 CortexMemory 实现**
   - 创建 `src/memory/cortex.rs`
   - 实现 `Memory` trait 的所有方法
   - 实现 CLI 执行和输出解析逻辑

3. **更新 Backend 枚举**
   - 在 `src/memory/backend.rs` 中添加 `MemoryBackendKind::Cortex`
   - 添加 `CORTEX_PROFILE`
   - 更新 `classify_memory_backend()` 和 `memory_backend_profile()`

4. **更新 Factory 函数**
   - 在 `src/memory/mod.rs` 中导入 `CortexMemory`
   - 在 `create_memory_with_storage_and_routes()` 中添加 Cortex 分支

### 阶段二: 核心功能完善 (2-3天)

5. **CLI 输出解析**
   - 实现 `parse_search_output()` - 解析搜索结果
   - 实现 `parse_list_output()` - 解析列表输出
   - 实现 `parse_stats_count()` - 解析统计信息
   - 添加单元测试

6. **会话管理集成**
   - 在 Agent 会话管理中集成 cortex-mem 的会话关闭逻辑
   - 实现自动触发内存提取

7. **错误处理和日志**
   - 添加详细的错误处理和上下文信息
   - 添加 tracing 日志记录
   - 实现 health_check 的真实逻辑

### 阶段三: 测试和文档 (1-2天)

8. **单元测试**
   - 为 `CortexMemory` 编写完整的单元测试
   - Mock CLI 执行进行测试

9. **集成测试**
   - 编写集成测试验证与真实 cortex-mem-cli 的交互
   - 测试各种场景:存储、搜索、删除等

10. **文档和示例**
    - 更新 `docs/config-reference.md` 添加 cortex backend 说明
    - 更新 `docs/memory-backend-guide.md` (如有) 或创建新文档
    - 添加示例配置文件

### 阶段四: 优化和增强 (可选)

11. **性能优化**
    - 实现 CLI 输出缓存
    - 批量操作支持
    - 并发请求限制

12. **高级功能**
    - 支持三层记忆查询(L0/L1/L2)
    - 支持语义相似度阈值配置
    - 支持租户切换

## 四、CLI 输出解析规范

需要根据 cortex-mem-cli 的实际输出格式设计解析逻辑。基于 README 中的示例,推测输出格式:

### search 命令输出格式

```
🔍 Searching for: what are the user's hobbies?
  📂 Scope: cortex://session/thread-123
  ⚙ Strategy: Vector Search

✓ Found 3 results

1. cortex://session/thread-123/memory-456.md (score: 0.89)
   The user mentioned they enjoy playing chess on weekends...

2. cortex://session/thread-123/memory-789.md (score: 0.76)
   Last week they talked about learning guitar...
```

### list 命令输出格式

```
📋 Listing memories from: cortex://session/thread-123

✓ Found 5 items:

📁 Directories (2):
  • timeline/
  • memories/

📄 Files (3):
  • memory-456.md
    1234 bytes
  • memory-789.md
    2567 bytes
```

### stats 命令输出格式

需要根据实际 CLI 输出确定格式。

## 五、依赖管理

### 5.1 Cargo.toml 更新

```toml
# Cargo.toml

[dependencies]
# ... 现有依赖 ...

# cortex-mem 集成不需要直接依赖 cortex-mem-core
# 只需要 CLI 工具可用

[dev-dependencies]
# 测试时可能需要 mock CLI 执行
tokio-test = "0.4"
```

### 5.2 cortex-mem 配置文件模板

为用户提供 `cortex-config.toml` 模板:

```toml
# workspace/cortex-config.toml

[qdrant]
url = "http://localhost:6334"
http_url = "http://localhost:6333"
collection_name = "zeroclaw-memory"
embedding_dim = 1536

[llm]
api_base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model_efficient = "gpt-4o-mini"
temperature = 0.7
max_tokens = 4096

[embedding]
api_base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model_name = "text-embedding-3-small"
batch_size = 32

[cortex]
data_dir = "./cortex-data"

[automation]
auto_index = true
auto_extract = true
```

## 六、迁移和兼容性

### 6.1 从现有 backend 迁移

提供迁移命令:

```bash
# 从 sqlite 迁移到 cortex
zeroclaw memory migrate --from sqlite --to cortex

# 实现思路:
# 1. 读取 sqlite 中的所有 memory
# 2. 为每个 memory 调用 cortex backend 的 store 方法
# 3. 验证迁移结果
```

### 6.2 后备机制

如果 cortex-mem-cli 不可用,fallback 到 markdown 或 sqlite:

```rust
pub fn create_memory(...) -> anyhow::Result<Box<dyn Memory>> {
    if backend == "cortex" {
        match CortexMemory::new(...) {
            Ok(mem) => return Ok(Box::new(mem)),
            Err(e) => {
                tracing::warn!("Failed to initialize cortex backend, falling back to sqlite: {}", e);
                // fallback to sqlite
            }
        }
    }
    // ... 其他 backend ...
}
```

## 七、测试策略

### 7.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[test]
    fn test_cortex_memory_new_success() {
        let tmp = TempDir::new().unwrap();
        let config = CortexMemConfig::default();
        // 需要 mock cortex-mem-cli
        let mem = CortexMemory::new(config, tmp.path());
        // 根据是否有 CLI 决定期望结果
    }
    
    #[tokio::test]
    async fn test_store_and_recall() {
        // 需要 mock CLI 执行
    }
}
```

### 7.2 集成测试

需要真实的 cortex-mem-cli 和 Qdrant 实例。

## 八、风险和缓解

### 8.1 依赖风险

**风险**: cortex-mem-cli 未安装或版本不兼容

**缓解**:
- 在初始化时检查 CLI 可用性
- 提供详细的错误信息和安装指南
- 实现 fallback 机制

### 8.2 性能风险

**风险**: CLI 调用开销较大

**缓解**:
- 批量操作支持
- 实现本地缓存
- 异步执行和超时控制

### 8.3 数据一致性风险

**风险**: cortex-mem 与 zeroclaw 数据模型不完全匹配

**缓解**:
- 实现数据模型映射层
- 详细的日志记录
- 提供数据迁移工具

## 九、后续增强

### 9.1 直接 API 集成(可选)

如果性能要求极高,可以考虑直接依赖 `cortex-mem-core` 和 `cortex-mem-tools`:

```rust
// 直接使用 MemoryOperations API
use cortex_mem_tools::MemoryOperations;

pub struct CortexMemoryDirect {
    operations: Arc<MemoryOperations>,
}

impl CortexMemoryDirect {
    pub async fn new(config: &CortexMemConfig) -> Result<Self> {
        let operations = MemoryOperations::new(
            &config.data_dir,
            &config.tenant,
            llm_client,
            &config.qdrant_url,
            // ...
        ).await?;
        
        Ok(Self { operations: Arc::new(operations) })
    }
}
```

### 9.2 混合模式

CLI-wrapper 模式和直接 API 模式可以共存,通过配置选择:

```toml
[memory.cortex]
mode = "cli"  # 或 "direct"
```

## 十、总结

本方案通过 **CLI-wrapper 模式** 实现了 cortex-mem 与 zeroclaw 的有效集成:

**核心优势:**
1. ✅ 松耦合架构,易于维护和升级
2. ✅ 完全遵循 zeroclaw 现有的 trait-based 架构
3. ✅ 支持三层记忆架构和语义搜索
4. ✅ 兼容现有会话管理机制
5. ✅ 提供 fallback 和迁移路径

**关键实现点:**
- 新增 `MemoryBackendKind::Cortex` 枚举值
- 实现 `CortexMemory` trait
- CLI 执行和输出解析
- 会话关闭触发内存提取

**开发估算:**
- 基础实现: 1-2 天
- 核心功能: 2-3 天
- 测试文档: 1-2 天
- 总计: 4-7 天

这个方案为 zeroclaw 提供了一个生产级的 cortex-mem 集成,既保持了架构的灵活性,又充分利用了 cortex-mem 的先进内存管理能力。
