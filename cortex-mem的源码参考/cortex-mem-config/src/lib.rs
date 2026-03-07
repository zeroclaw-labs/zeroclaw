use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Main configuration structure (V2 - simplified)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub qdrant: QdrantConfig,
    pub embedding: EmbeddingConfig,
    pub llm: LLMConfig,
    pub server: ServerConfig,
    pub logging: LoggingConfig,
    pub cortex: CortexConfig,
}

/// Cortex Memory configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexConfig {
    /// Data directory for Cortex Memory
    /// If not specified, will use system application data directory
    #[serde(default)]
    pub data_dir: Option<String>,
}

impl CortexConfig {
    /// Get the effective data directory
    pub fn data_dir(&self) -> String {
        self.data_dir.clone().unwrap_or_else(|| {
            Self::default_data_dir()
        })
    }
    
    /// Get the default data directory
    fn default_data_dir() -> String {
        // 优先级：
        // 1. 环境变量 CORTEX_DATA_DIR
        // 2. 应用数据目录/cortex (TARS 应用)
        // 3. 当前目录 ./.cortex
        std::env::var("CORTEX_DATA_DIR")
            .ok()
            .or_else(|| {
                // 尝试使用应用数据目录（TARS 默认路径）
                directories::ProjectDirs::from("com", "cortex-mem", "tars")
                    .map(|dirs| {
                        let cortex_dir = dirs.data_dir().join("cortex");
                        cortex_dir.to_string_lossy().to_string()
                    })
            })
            .unwrap_or_else(|| "./.cortex".to_string())
    }
}

/// Qdrant vector database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantConfig {
    pub url: String,
    pub collection_name: String,
    pub embedding_dim: Option<usize>,
    pub timeout_secs: u64,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Embedding configuration for vector search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    pub api_base_url: String,
    pub api_key: String,
    pub model_name: String,
    pub batch_size: usize,
    pub timeout_secs: u64,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        EmbeddingConfig {
            api_base_url: std::env::var("EMBEDDING_API_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            api_key: std::env::var("EMBEDDING_API_KEY")
                .unwrap_or_else(|_| "".to_string()),
            model_name: std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".to_string()),
            batch_size: 10,
            timeout_secs: 30,
        }
    }
}

/// LLM configuration for rig framework
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMConfig {
    pub api_base_url: String,
    pub api_key: String,
    pub model_efficient: String,
    pub temperature: f32,
    pub max_tokens: u32,
}

/// HTTP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub cors_origins: Vec<String>,
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub enabled: bool,
    pub log_directory: String,
    pub level: String,
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            enabled: false,
            log_directory: "logs".to_string(),
            level: "info".to_string(),
        }
    }
}

impl Default for CortexConfig {
    fn default() -> Self {
        CortexConfig {
            data_dir: None,  // Use None to trigger smart default
        }
    }
}
