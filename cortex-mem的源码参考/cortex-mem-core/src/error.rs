use thiserror::Error;

/// Cortex-Mem error types
#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid URI: {0}")]
    InvalidUri(String),
    
    #[error("Invalid URI scheme, expected 'cortex://'")]
    InvalidScheme,
    
    #[error("Invalid dimension: {0}")]
    InvalidDimension(String),
    
    #[error("Invalid path in URI")]
    InvalidPath,
    
    #[error("Memory not found: {uri}")]
    NotFound { uri: String },
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    
    #[error("LLM error: {0}")]
    Llm(String),
    
    #[error("Embedding error: {0}")]
    Embedding(String),
    
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Vector store error: {0}")]
    VectorStore(#[from] qdrant_client::QdrantError),
    
    #[error("{0}")]
    Other(String),
}

/// Result type alias
pub type Result<T> = std::result::Result<T, Error>;