# Cortex Memory Tools Library

`cortex-mem-tools` provides high-level abstractions and utilities for working with the Cortex Memory system. It offers simplified APIs for common operations three tiered access (L0/L1/L2 layers).

## 🛠️ Overview

Cortex Memory Tools implements:
- High-level `MemoryOperations` interface for unified access to Cortex Memory
- **Tiered Access**: L0 (Abstract ~100 tokens), L1 (Overview ~2000 tokens), L2 (Full Content)
- Advanced automation with event-driven processing
- Model Context Protocol (MCP) tool definitions
- Type-safe error handling and comprehensive types

## 🏗️ Core Components

### MemoryOperations

The primary interface for working with Cortex Memory:

```
MemoryOperations
       |
       +---> Tiered Access (L0/L1/L2)
       |         |
       |         +---> get_abstract()  -> AbstractResponse
       |         +---> get_overview()  -> OverviewResponse
       |         +---> get_read()      -> ReadResponse
       |
       +---> Search Operations
       |         |
       |         +---> search()  -> SearchResponse
       |         +---> find()    -> FindResponse
       |
       +---> Filesystem Operations
       |         |
       |         +---> ls()      -> LsResponse
       |         +---> explore() -> ExploreResponse
       |
       +---> Storage Operations
       |         |
       |         +---> store()   -> StoreResponse
       |
       +---> Session Management
       |         |
       |         +---> add_message()
       |         +---> list_sessions()
       |         +---> get_session()
       |         +---> close_session()
       |
       +---> Automation
                 |
                 +---> ensure_all_layers()
                 +---> index_all_files()
```

## 🚀 Quick Start

### Installation

```toml
[dependencies]
cortex-mem-tools = { path = "../cortex-mem-tools" }
cortex-mem-core = { path = "../cortex-mem-core" }
tokio = { version = "1", features = ["full"] }
```

### Basic Usage

```rust
use cortex_mem_tools::MemoryOperations;
use cortex_mem_core::llm::{LLMClientImpl, LLMConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create LLM client
    let llm_config = LLMConfig {
        api_base_url: "https://api.openai.com/v1".to_string(),
        api_key: "your-api-key".to_string(),
        model_efficient: "gpt-4o-mini".to_string(),
        temperature: 0.1,
        max_tokens: 4096,
    };
    let llm_client = Arc::new(LLMClientImpl::new(llm_config)?);
    
    // Create MemoryOperations with all dependencies
    let ops = MemoryOperations::new(
        "./cortex-data",           // data directory
        "default",                  // tenant ID
        llm_client,                 // LLM client
        "http://localhost:6333",    // Qdrant URL
        "cortex_memories",          // Qdrant collection
        "https://api.openai.com/v1", // Embedding API URL
        "your-embedding-key",       // Embedding API key
        "text-embedding-3-small",   // Embedding model
        Some(1536),                 // Embedding dimension
        None,                       // Optional user ID
    ).await?;
    
    // Add a message to a session
    let msg_id = ops.add_message(
        "tech-support",
        "user",
        "How do I reset my password?"
    ).await?;
    println!("Message ID: {}", msg_id);
    
    // Get abstract (L0) - quick relevance check
    let abstract_result = ops.get_abstract(&format!(
        "cortex://session/tech-support/timeline/{}.md", 
        msg_id
    )).await?;
    println!("Abstract: {}", abstract_result.abstract_text);
    
    // Get overview (L1) - partial context
    let overview = ops.get_overview(&format!(
        "cortex://session/tech-support/timeline/{}.md",
        msg_id
    )).await?;
    println!("Overview: {}", overview.overview_text);
    
    // Read file content (L2)
    let content = ops.read_file(&format!(
        "cortex://session/tech-support/timeline/{}.md",
        msg_id
    )).await?;
    println!("Content: {}", content);
    
    // List sessions
    let sessions = ops.list_sessions().await?;
    for session in sessions {
        println!("Session: {} ({})", session.thread_id, session.status);
    }
    
    Ok(())
}
```

### Tiered Access

| Layer | Size | Purpose | Method |
|-------|------|---------|--------|
| **L0 Abstract** | ~100 tokens | Quick relevance judgment | `get_abstract()` |
| **L1 Overview** | ~2000 tokens | Partial context understanding | `get_overview()` |
| **L2 Full** | Complete content | Deep analysis and processing | `read_file()` / `get_read()` |

### Tool-Based Operations

```rust
use cortex_mem_tools::{MemoryOperations, SearchArgs, LsArgs, StoreArgs};

// Search with typed args
let search_result = ops.search(SearchArgs {
    query: "password reset".to_string(),
    recursive: Some(true),
    return_layers: Some(vec!["L0".to_string(), "L1".to_string()]),
    scope: Some("cortex://session".to_string()),
    limit: Some(10),
}).await?;

for result in &search_result.results {
    println!("URI: {} (score: {:.2})", result.uri, result.score);
}

// List directory with abstracts
let ls_result = ops.ls(LsArgs {
    uri: "cortex://session".to_string(),
    recursive: Some(false),
    include_abstracts: Some(true),
}).await?;

for entry in &ls_result.entries {
    println!("{}: {}", entry.name, entry.abstract_text.as_ref().unwrap_or(&"".to_string()));
}

// Store with auto layer generation
let store_result = ops.store(StoreArgs {
    content: "User prefers dark mode and vim keybindings".to_string(),
    thread_id: "user-prefs".to_string(),
    metadata: None,
    auto_generate_layers: Some(true),
    scope: "user".to_string(),
    user_id: Some("user-123".to_string()),
    agent_id: None,
}).await?;

println!("Stored at: {}", store_result.uri);
```

## 📚 API Reference

### MemoryOperations Constructor

```rust
impl MemoryOperations {
    /// Create with full dependencies (primary constructor)
    pub async fn new(
        data_dir: &str,
        tenant_id: impl Into<String>,
        llm_client: Arc<dyn LLMClient>,
        qdrant_url: &str,
        qdrant_collection: &str,
        embedding_api_base_url: &str,
        embedding_api_key: &str,
        embedding_model_name: &str,
        embedding_dim: Option<usize>,
        user_id: Option<String>,
    ) -> Result<Self>
}
```

### Accessor Methods

| Method | Return Type | Description |
|--------|-------------|-------------|
| `filesystem()` | `&Arc<CortexFilesystem>` | Get underlying filesystem |
| `vector_engine()` | `&Arc<VectorSearchEngine>` | Get vector search engine |
| `session_manager()` | `&Arc<RwLock<SessionManager>>` | Get session manager |
| `auto_extractor()` | `Option<&Arc<AutoExtractor>>` | Get auto extractor |
| `layer_generator()` | `Option<&Arc<LayerGenerator>>` | Get layer generator |
| `auto_indexer()` | `Option<&Arc<AutoIndexer>>` | Get auto indexer |

### Session Management

| Method | Parameters | Description |
|--------|------------|-------------|
| `add_message()` | `thread_id, role, content` | Add message to session |
| `list_sessions()` | - | List all sessions |
| `get_session()` | `thread_id` | Get session info |
| `close_session()` | `thread_id` | Close a session |

### Tiered Access (L0/L1/L2)

| Method | Parameters | Returns |
|--------|------------|---------|
| `get_abstract()` | `uri: &str` | `AbstractResponse` |
| `get_overview()` | `uri: &str` | `OverviewResponse` |
| `get_read()` | `uri: &str` | `ReadResponse` |

### File Operations

| Method | Parameters | Description |
|--------|------------|-------------|
| `read_file()` | `uri` | Read file content |
| `list_files()` | `uri` | List files in directory |
| `delete()` | `uri` | Delete file/directory |
| `exists()` | `uri` | Check existence |

### Tool-Based Operations

| Method | Parameters | Returns |
|--------|------------|---------|
| `search()` | `SearchArgs` | `SearchResponse` |
| `find()` | `FindArgs` | `FindResponse` |
| `ls()` | `LsArgs` | `LsResponse` |
| `explore()` | `ExploreArgs` | `ExploreResponse` |
| `store()` | `StoreArgs` | `StoreResponse` |

### Automation Methods

| Method | Return Type | Description |
|--------|-------------|-------------|
| `ensure_all_layers()` | `GenerationStats` | Generate missing L0/L1 layers |
| `index_all_files()` | `SyncStats` | Index all files to vector DB |

## 📖 Type Definitions

### Tiered Access Responses

```rust
pub struct AbstractResponse {
    pub uri: String,
    pub abstract_text: String,
    pub layer: String,       // "L0"
    pub token_count: usize,
}

pub struct OverviewResponse {
    pub uri: String,
    pub overview_text: String,
    pub layer: String,       // "L1"
    pub token_count: usize,
}

pub struct ReadResponse {
    pub uri: String,
    pub content: String,
    pub layer: String,       // "L2"
    pub token_count: usize,
    pub metadata: Option<FileMetadata>,
}
```

### Search Types

```rust
pub struct SearchArgs {
    pub query: String,
    pub recursive: Option<bool>,           // Default: true
    pub return_layers: Option<Vec<String>>, // ["L0", "L1", "L2"]
    pub scope: Option<String>,              // Search scope URI
    pub limit: Option<usize>,               // Default: 10
}

pub struct SearchResponse {
    pub query: String,
    pub results: Vec<SearchResult>,
    pub total: usize,
    pub engine_used: String,
}

pub struct SearchResult {
    pub uri: String,
    pub score: f32,
    pub snippet: String,
    pub content: Option<String>,
}

pub struct FindArgs {
    pub query: String,
    pub scope: Option<String>,
    pub limit: Option<usize>,
}

pub struct FindResponse {
    pub query: String,
    pub results: Vec<FindResult>,
    pub total: usize,
}
```

### Filesystem Types

```rust
pub struct LsArgs {
    pub uri: String,                        // Default: "cortex://session"
    pub recursive: Option<bool>,            // Default: false
    pub include_abstracts: Option<bool>,    // Default: false
}

pub struct LsResponse {
    pub uri: String,
    pub entries: Vec<LsEntry>,
    pub total: usize,
}

pub struct ExploreArgs {
    pub query: String,
    pub start_uri: Option<String>,          // Default: "cortex://session"
    pub max_depth: Option<usize>,           // Default: 3
    pub return_layers: Option<Vec<String>>, // Default: ["L0"]
}

pub struct ExploreResponse {
    pub query: String,
    pub exploration_path: Vec<String>,
    pub matches: Vec<ExploreMatch>,
    pub total_explored: usize,
    pub total_matches: usize,
}
```

### Storage Types

```rust
pub struct StoreArgs {
    pub content: String,
    pub thread_id: String,                  // Default: ""
    pub metadata: Option<Value>,
    pub auto_generate_layers: Option<bool>, // Default: true
    pub scope: String,                      // "session", "user", or "agent"
    pub user_id: Option<String>,            // Required for user scope
    pub agent_id: Option<String>,           // Required for agent scope
}

pub struct StoreResponse {
    pub uri: String,
    pub layers_generated: Vec<String>,
    pub success: bool,
}
```

### Session Info

```rust
pub struct SessionInfo {
    pub thread_id: String,
    pub status: String,
    pub message_count: usize,
    pub created_at: String,
    pub updated_at: String,
}
```

## 🔌 MCP Integration

The library provides tool definitions for Model Context Protocol:

```rust
use cortex_mem_tools::mcp::{get_mcp_tool_definitions, get_mcp_tool_definition};

// Get all available MCP tool definitions
let tools = get_mcp_tool_definitions();
for tool in &tools {
    println!("Tool: {} - {}", tool.name, tool.description);
}

// Get a specific tool definition
if let Some(tool) = get_mcp_tool_definition("search") {
    println!("Search tool schema: {:?}", tool.input_schema);
}
```

### Available MCP Tools

| Tool | Description | Required Params | Optional Params |
|------|-------------|-----------------|-----------------|
| `abstract` | Get L0 abstract | `uri` | - |
| `overview` | Get L1 overview | `uri` | - |
| `read` | Get L2 full content | `uri` | - |
| `search` | Intelligent search | `query` | `recursive, return_layers, scope, limit` |
| `find` | Quick search (L0 only) | `query` | `scope, limit` |
| `ls` | List directory | `uri` | `recursive, include_abstracts` |
| `explore` | Intelligent exploration | `query` | `start_uri, max_depth, return_layers` |
| `store` | Store with auto layers | `content, thread_id` | `metadata, auto_generate_layers, scope, user_id, agent_id` |

## ⚠️ Error Handling

All operations return `Result<T, ToolsError>`:

```rust
pub enum ToolsError {
    InvalidInput(String),      // Invalid input provided
    Runtime(String),           // Runtime error during operation
    NotFound(String),          // Memory not found
    Custom(String),            // Custom error
    Serialization(Error),      // Serde JSON error
    Core(Error),               // Core library error
    Io(Error),                 // IO error
}

pub type Result<T> = std::result::Result<T, ToolsError>;
```

## 📦 Dependencies

- `cortex-mem-core` - Core library with all memory operations
- `tokio` - Async runtime
- `serde` / `serde_json` - Serialization
- `anyhow` / `thiserror` - Error handling
- `tracing` - Logging
- `chrono` - Date/time handling
- `uuid` - Unique identifiers
- `async-trait` - Async trait support

## 🧪 Testing

```bash
# Run tests
cargo test -p cortex-mem-tools

# Run specific tests
cargo test -p cortex-mem-tools core_functionality
```

## 📄 License

MIT License - see the [LICENSE](../../LICENSE) file for details.

## 🤝 Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch
3. Add comprehensive tests
4. Submit a pull request

## 🔗 Related Crates

- [`cortex-mem-core`](../cortex-mem-core/) - Core library
- [`cortex-mem-mcp`](../cortex-mem-mcp/) - MCP server implementation
- [`cortex-mem-rig`](../cortex-mem-rig/) - Rig framework integration
- [`cortex-mem-service`](../cortex-mem-service/) - HTTP REST API
- [`cortex-mem-cli`](../cortex-mem-cli/) - Command-line interface

---

**Built with ❤️ using Rust**
