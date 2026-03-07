# Cortex Memory Configuration Library

`cortex-mem-config` is the configuration management library for the Cortex Memory system, providing a centralized and flexible way to handle all configuration aspects through environment variables, configuration files, and default values.

## 🧩 Overview

Cortex Memory Configuration implements:
- TOML-based configuration file support
- Environment variable fallback system with sensible defaults
- Structured configuration for all Cortex components (Qdrant, LLM, embeddings)
- Platform-aware data directory resolution
- Configuration validation and merging

## 🏗️ Configuration Structure

### Core Components

The configuration is divided into several sections:

| Section | Purpose | Example |
|---------|---------|---------|
| **`cortex`** | Data storage and core settings | `data_dir: "./cortex-data"` |
| **`qdrant`** | Vector database connection | `url: "http://localhost:6333"` |
| **`embedding`** | Embedding generation API | `model_name: "text-embedding-3-small"` |
| **`llm`** | Large language model settings | `model_efficient: "gpt-4o-mini"` |
| **`server`** | HTTP server configuration | `host: "127.0.0.1", port: 8080` |
| **`logging`** | Logging configuration | `level: "info"` |

## 🚀 Quick Start

### Using Default Configuration

```rust
use cortex_mem_config::Config;

// Load from environment or use defaults
let config = Config::from_env()?;
println!("Data directory: {}", config.cortex.data_dir());
```

### Using Configuration File

```rust
use cortex_mem_config::Config;

// Load from config file
let config = Config::from_file("config.toml")?;
println!("Qdrant URL: {}", config.qdrant.url);
```

### Programmatic Configuration

```rust
use cortex_mem_config::{Config, QdrantConfig, LLMConfig, EmbeddingConfig};

let config = Config {
    qdrant: QdrantConfig {
        url: "http://localhost:6333".to_string(),
        collection_name: "cortex_memories".to_string(),
        embedding_dim: Some(1536),
        timeout_secs: 30,
    },
    // ... other sections
};
```

## 🌐 Data Directory Resolution

Cortex follows this priority order for determining the data directory:

1. **CORTEX_DATA_DIR** environment variable
2. Platform-specific application data directory (e.g., `~/Library/Application Support/cortex-mem/tars/cortex`)
3. Current directory `./.cortex` as fallback

### Example Resolutions

| Platform | Path |
|----------|------|
| **macOS** | `~/Library/Application Support/cortex-mem/tars/cortex` |
| **Linux** | `~/.local/share/cortex-mem/tars/cortex` |
| **Windows** | `%APPDATA%\cortex-mem\tars\cortex` |

## 📝 Configuration File Format

Example `config.toml`:

```toml
# Cortex Memory Configuration

[cortex]
# Data directory (optional - will use platform default)
data_dir = "/opt/cortex/data"

[qdrant]
# Vector database connection
url = "http://localhost:6333"
collection_name = "cortex_memories"
embedding_dim = 1536
timeout_secs = 30

[embedding]
# Embedding generation API
api_base_url = "https://api.openai.com/v1"
api_key = "${EMBEDDING_API_KEY}"
model_name = "text-embedding-3-small"
batch_size = 10
timeout_secs = 30

[llm]
# Large language model settings
api_base_url = "https://api.openai.com/v1"
api_key = "${LLM_API_KEY}"
model_efficient = "gpt-4o-mini"
temperature = 0.7
max_tokens = 4096
timeout_secs = 60

[server]
# HTTP server configuration
host = "127.0.0.1"
port = 8080

[logging]
# Logging configuration
level = "info"
```

## 🔧 Environment Variables

Cortex respects these environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| **CORTEX_DATA_DIR** | Override data directory | Platform-specific |
| **QDRANT_URL** | Qdrant server URL | `http://localhost:6333` |
| **QDRANT_COLLECTION** | Qdrant collection name | `cortex_memories` |
| **LLM_API_BASE_URL** | LLM API endpoint | `https://api.openai.com/v1` |
| **LLM_API_KEY** | LLM API authentication key | - |
| **LLM_MODEL** | LLM model name | `gpt-4o-mini` |
| **EMBEDDING_API_BASE_URL** | Embedding API endpoint | `https://api.openai.com/v1` |
| **EMBEDDING_API_KEY** | Embedding API key | - |
| **EMBEDDING_MODEL** | Embedding model | `text-embedding-3-small` |

## 🔄 Configuration Merging

Configuration follows this precedence (highest to lowest):

1. **Environment variables** (runtime overrides)
2. **Configuration file** (persistent settings)
3. **Default values** (fallbacks)

## 🧱 Usage Patterns

### Service Configuration

```rust
use cortex_mem_config::Config;

// In service initialization
let config = Config::from_env().or_else(|_| Config::from_file("config.toml"))?;

// Initialize qdrant client
let qdrant_client = qdrant_client::Qdrant::new(
    &config.qdrant.url,
    config.qdrant.timeout_secs
)?;

// Initialize LLM client
let llm_client = rig::providers::openai::Client::new(&config.llm.api_key);
```

### CLI Application Configuration

```rust
use cortex_mem_config::Config;

// Load with overrides
let mut config = Config::from_env()?;

// Apply command-line overrides
if let Some(data_dir) = cli_args.data_dir {
    config.cortex.data_dir = Some(data_dir);
}

if let Some(port) = cli_args.port {
    config.server.port = port;
}
```

## 📚 API Reference

### Main Types

- **`Config`**: Root configuration structure
- **`QdrantConfig`**: Vector database settings
- **`EmbeddingConfig`**: Embedding generation settings
- **`LLMConfig`**: Language model settings
- **`ServerConfig`**: HTTP server settings
- **`LoggingConfig`**: Logging configuration

### Key Methods

```rust
impl Config {
    // Load from environment variables
    pub fn from_env() -> Result<Config>
    
    // Load from TOML file
    pub fn from_file(path: impl AsRef<Path>) -> Result<Config>
    
    // Load from string
    pub fn from_toml(toml: &str) -> Result<Config>
    
    // Validate configuration
    pub fn validate(&self) -> Result<()>
}
```

## 🔧 Configuration Validation

The library validates configuration at load time:

- **Required fields**: API keys for LLM and embedding services must be present
- **URLs**: Must be valid URLs
- **Ports**: Must be in valid range (1-65535)
- **Paths**: Data directory must be accessible

## 📄 License

MIT License - see the [`LICENSE`](../../LICENSE) file for details.

## 🤝 Contributing

Please read our contributing guidelines and submit pull requests to the main repository.

## 🔍 See Also

- [Cortex Memory Core](../cortex-mem-core/README.md)
- [Cortex Memory Service](../cortex-mem-service/README.md)
- [Configuration Examples](examples/) directory