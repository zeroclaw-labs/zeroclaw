# Cortex Memory Core Library

`cortex-mem-core` is the foundational library of the Cortex Memory system, providing core services and abstractions for AI agent memory management.

## 🧠 Overview

Cortex Memory Core implements:
- A virtual filesystem with `cortex://` URI scheme for memory storage
- Three-tier memory architecture (L0/L1/L2 layers)
- Session-based conversational memory management
- Vector search integration with Qdrant
- LLM-based memory extraction and profiling
- Event-driven automation system

## 🏗️ Architecture

### Core Modules

| Module | Purpose | Key Components |
|--------|---------|----------------|
| **`filesystem`** | Virtual file system with custom URI scheme | `CortexFilesystem`, `CortexUri`, `FilesystemOperations` |
| **`session`** | Conversational session management | `SessionManager`, `Message`, `TimelineGenerator`, `ParticipantManager` |
| **`vector_store`** | Vector database abstraction | `VectorStore` trait, `QdrantVectorStore` |
| **`search`** | Semantic and layered search engines | `VectorSearchEngine`, `SearchOptions`, `SearchResult` |
| **`extraction`** | Memory extraction and profiling | `MemoryExtractor`, `ExtractedMemories` |
| **`automation`** | Event-driven automation | `AutomationManager`, `AutoIndexer`, `AutoExtractor`, `LayerGenerator` |
| **`layers`** | Three-tier memory architecture | `LayerManager`, `ContextLayer` |
| **`llm`** | Large language model abstraction | `LLMClient` trait, `LLMClientImpl` |
| **`embedding`** | Embedding generation | `EmbeddingClient`, `EmbeddingCache` |
| **`events`** | Event system for automation | `CortexEvent`, `EventBus` |
| **`builder`** | Unified initialization API | `CortexMemBuilder`, `CortexMem` |

## 🚀 Quick Start

### Using CortexMemBuilder (Recommended)

```rust
use cortex_mem_core::{CortexMemBuilder, LLMConfig, QdrantConfig, EmbeddingConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cortex = CortexMemBuilder::new("./cortex-data")
        .with_embedding(EmbeddingConfig {
            api_base_url: "https://api.openai.com/v1".to_string(),
            api_key: "your-api-key".to_string(),
            model_name: "text-embedding-3-small".to_string(),
            batch_size: 10,
            timeout_secs: 30,
        })
        .with_qdrant(QdrantConfig {
            url: "http://localhost:6333".to_string(),
            collection_name: "cortex_memories".to_string(),
            embedding_dim: 1536,
            timeout_secs: 30,
            tenant_id: "default".to_string(),
        })
        .build()
        .await?;

    // Access components
    let session_manager = cortex.session_manager();
    let filesystem = cortex.filesystem();
    let vector_store = cortex.vector_store();

    Ok(())
}
```

### Basic Filesystem Usage

```rust
use cortex_mem_core::{CortexFilesystem, FilesystemOperations};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize filesystem
    let fs = Arc::new(CortexFilesystem::new("./cortex-data"));
    fs.initialize().await?;

    // Write a memory
    fs.write("cortex://user/john/preferences.md", 
             "Prefers dark mode and vim keybindings").await?;

    // Read back
    let content = fs.read("cortex://user/john/preferences.md").await?;
    println!("Content: {}", content);

    // List directory
    let entries = fs.list("cortex://user/john").await?;
    for entry in entries {
        println!("{}: {} ({})", entry.name, entry.uri, 
                 if entry.is_directory { "dir" } else { "file" });
    }

    Ok(())
}
```

### Session Management

```rust
use cortex_mem_core::{SessionManager, SessionConfig, Message, MessageRole, CortexFilesystem};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fs = Arc::new(CortexFilesystem::new("./cortex-data")?);
    fs.initialize().await?;
    
    let session_manager = SessionManager::new(fs, SessionConfig::default());
    
    // Create a session
    let session = session_manager.create_session("tech-support").await?;
    
    // Add messages
    session_manager.add_message(
        &session.thread_id,
        "user",
        "How do I reset my password?"
    ).await?;
    
    // List sessions
    let sessions = session_manager.list_sessions().await?;
    for s in sessions {
        println!("Session: {} ({:?})", s.thread_id, s.status);
    }
    
    Ok(())
}
```

### Vector Search

```rust
use cortex_mem_core::{VectorSearchEngine, SearchOptions};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let search_engine = VectorSearchEngine::new(
        qdrant_store,
        embedding_client,
        filesystem
    );
    
    // Basic semantic search
    let results = search_engine.semantic_search(
        "password reset",
        SearchOptions {
            limit: 10,
            threshold: 0.5,
            root_uri: Some("cortex://session".to_string()),
            recursive: true,
        }
    ).await?;
    
    // Layered semantic search (L0 -> L1 -> L2)
    let layered_results = search_engine.layered_semantic_search(
        "password reset",
        SearchOptions::default()
    ).await?;
    
    for result in layered_results {
        println!("Found: {} (score: {:.2})", result.uri, result.score);
    }
    
    Ok(())
}
```

## 🌐 The Cortex Filesystem

The Cortex Filesystem extends standard file operations with custom URIs:

### URI Scheme

```
cortex://{dimension}/{category}/{subcategory}/{resource}
```

### Dimensions and Categories

| Dimension | Categories | Description |
|-----------|------------|-------------|
| **`session`** | `{session-id}/timeline` | Conversational sessions with timeline |
| **`user`** | `preferences`, `entities`, `events` | User-specific memories |
| **`agent`** | `cases`, `skills`, `instructions` | Agent-specific memories |
| **`resources`** | Various | Shared resources |

### Example URIs

```
cortex://session/tech-support/timeline/2024/01/15/14_30_00_abc123.md
cortex://user/john/preferences.md
cortex://agent/assistant/skills/rust-programming.md
cortex://resources/templates/meeting-notes.md
```

## 📚 Memory Architecture (Three-Tier System)

Cortex implements a three-tier memory system:

| Layer | Size | Purpose | File Suffix |
|-------|------|---------|-------------|
| **L0 Abstract** | ~100 tokens | Ultra-condensed summaries, quick relevance check | `.abstract.md` |
| **L1 Overview** | ~500-2000 tokens | Detailed summaries, key points and decisions | `.overview.md` |
| **L2 Detail** | Full content | Complete original content, source of truth | `.md` (original) |

### Layer Generation

```rust
use cortex_mem_core::layers::LayerManager;

let layer_manager = LayerManager::new(filesystem, llm_client);

// Generate all layers for content
let layers = layer_manager.generate_all_layers("cortex://session/.../message.md", &content).await?;

// Load specific layer
let abstract_content = layer_manager.load("cortex://session/.../message.md", ContextLayer::L0Abstract).await?;
```

## 📖 API Reference

### Core Types

```rust
// Dimension enum
pub enum Dimension {
    Resources,
    User,
    Agent,
    Session,
}

// Context layers
pub enum ContextLayer {
    L0Abstract,   // ~100 tokens
    L1Overview,   // ~500-2000 tokens
    L2Detail,     // Full content
}

// Memory types
pub enum MemoryType {
    Conversational,
    Procedural,
    Semantic,
    Episodic,
}

// User memory categories
pub enum UserMemoryCategory {
    Profile,
    Preferences,
    Entities,
    Events,
}

// Agent memory categories
pub enum AgentMemoryCategory {
    Cases,
    Skills,
    Instructions,
}

// File entry
pub struct FileEntry {
    pub uri: String,
    pub name: String,
    pub is_directory: bool,
    pub size: Option<u64>,
    pub modified: Option<DateTime<Utc>>,
}

// Memory with embedding
pub struct Memory {
    pub id: String,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub metadata: MemoryMetadata,
}

// Search result
pub struct ScoredMemory {
    pub memory: Memory,
    pub score: f32,
}
```

### FilesystemOperations Trait

```rust
#[async_trait]
pub trait FilesystemOperations: Send + Sync {
    async fn list(&self, uri: &str) -> Result<Vec<FileEntry>>;
    async fn read(&self, uri: &str) -> Result<String>;
    async fn write(&self, uri: &str, content: &str) -> Result<()>;
    async fn delete(&self, uri: &str) -> Result<()>;
    async fn exists(&self, uri: &str) -> Result<bool>;
    async fn metadata(&self, uri: &str) -> Result<FileMetadata>;
}
```

### VectorStore Trait

```rust
#[async_trait]
pub trait VectorStore: Send + Sync + DynClone {
    async fn insert(&self, memory: &Memory) -> Result<()>;
    async fn search(&self, query_vector: &[f32], filters: &Filters, limit: usize) -> Result<Vec<ScoredMemory>>;
    async fn search_with_threshold(&self, query_vector: &[f32], filters: &Filters, limit: usize, score_threshold: f32) -> Result<Vec<ScoredMemory>>;
    async fn update(&self, memory: &Memory) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Memory>>;
    async fn list(&self, filters: &Filters, limit: Option<usize>) -> Result<Vec<Memory>>;
    async fn health_check(&self) -> Result<bool>;
}
```

### LLMClient Trait

```rust
#[async_trait]
pub trait LLMClient: Send + Sync {
    async fn complete(&self, prompt: &str) -> Result<String>;
    async fn complete_with_system(&self, system: &str, prompt: &str) -> Result<String>;
    async fn extract_memories(&self, prompt: &str) -> Result<MemoryExtractionResponse>;
    async fn extract_structured_facts(&self, prompt: &str) -> Result<StructuredFactExtraction>;
    async fn extract_detailed_facts(&self, prompt: &str) -> Result<DetailedFactExtraction>;
    fn model_name(&self) -> &str;
    fn config(&self) -> &LLMConfig;
}
```

## 🔧 Configuration

### QdrantConfig

```rust
pub struct QdrantConfig {
    pub url: String,              // Default: "http://localhost:6333"
    pub collection_name: String,  // Default: "cortex_memories"
    pub embedding_dim: usize,     // Default: 1536
    pub timeout_secs: u64,        // Default: 30
    pub tenant_id: String,        // Default: "default"
}
```

### EmbeddingConfig

```rust
pub struct EmbeddingConfig {
    pub api_base_url: String,     // Default: OpenAI API
    pub api_key: String,          // From EMBEDDING_API_KEY or LLM_API_KEY
    pub model_name: String,       // Default: "text-embedding-3-small"
    pub batch_size: usize,        // Default: 10
    pub timeout_secs: u64,        // Default: 30
}
```

### LLMConfig

```rust
pub struct LLMConfig {
    pub api_base_url: String,     // Default: OpenAI API
    pub api_key: String,          // From LLM_API_KEY env var
    pub model_efficient: String,  // Default: "gpt-3.5-turbo"
    pub temperature: f32,         // Default: 0.1
    pub max_tokens: usize,        // Default: 4096
}
```

### SessionConfig

```rust
pub struct SessionConfig {
    pub auto_extract_on_close: bool,           // Default: true
    pub max_messages_per_session: Option<usize>,
    pub auto_archive_after_days: Option<i64>,
}
```

### AutomationConfig

```rust
pub struct AutomationConfig {
    pub auto_index: bool,                      // Default: true
    pub auto_extract: bool,                    // Default: true
    pub index_on_message: bool,                // Default: false
    pub index_on_close: bool,                  // Default: true
    pub index_batch_delay: u64,                // Default: 2 seconds
    pub auto_generate_layers_on_startup: bool, // Default: false
}
```

## 🔄 Event System

Cortex includes an event-driven automation system:

```rust
use cortex_mem_core::{CortexEvent, EventBus, AutomationManager};

// Create event bus
let (event_tx, event_rx) = EventBus::new();

// Publish events
event_tx.publish(CortexEvent::Session(SessionEvent::MessageAdded {
    thread_id: "tech-support".to_string(),
    message_id: "msg-123".to_string(),
}));

// Handle events in automation manager
match event {
    CortexEvent::Session(event) => {
        // Session event - trigger extraction/indexing
    }
    CortexEvent::Filesystem(event) => {
        // File changed - trigger re-indexing
    }
}
```

### Event Types

```rust
pub enum CortexEvent {
    Session(SessionEvent),
    Filesystem(FilesystemEvent),
}

pub enum SessionEvent {
    Created { thread_id: String },
    MessageAdded { thread_id: String, message_id: String },
    Closed { thread_id: String },
}

pub enum FilesystemEvent {
    FileCreated { uri: String },
    FileModified { uri: String },
    FileDeleted { uri: String },
}
```

## 🔗 Integration with Other Crates

- **`cortex-mem-config`**: Configuration loading and management
- **`cortex-mem-tools`**: High-level utilities and MCP tool definitions
- **`cortex-mem-rig`**: Rig framework adapters
- **`cortex-mem-service`**: REST API implementation
- **`cortex-mem-cli`**: Command-line interface
- **`cortex-mem-mcp`**: MCP server implementation
- **`cortex-mem-insights`**: Observability dashboard

## 🧪 Testing

Running tests requires all features:

```bash
cargo test -p cortex-mem-core --all-features
```

## 📦 Dependencies

Key dependencies include:
- `serde` / `serde_json` for serialization
- `tokio` for async runtime
- `qdrant-client` for vector storage
- `rig-core` for LLM integration
- `chrono` for timestamps
- `uuid` for unique identifiers
- `regex` for text matching
- `sha2` for hashing
- `tracing` for logging
- `reqwest` for HTTP requests

## 📄 License

MIT License - see the [`LICENSE`](../../LICENSE) file for details.

## 🤝 Contributing

Please read our contributing guidelines and submit pull requests to the main repository.

## 🔍 Additional Documentation

- [Architecture Overview](../../litho.docs/en/2.Architecture.md)
- [Core Workflow](../../litho.docs/en/3.Workflow.md)
- [System Boundaries](../../litho.docs/en/5.Boundary-Interfaces.md)