use thiserror::Error;

/// Common error types for memory tools
#[derive(Debug, Error)]
pub enum ToolsError {
    /// Invalid input provided
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Runtime error during operation
    #[error("Runtime error: {0}")]
    Runtime(String),

    /// Memory not found
    #[error("Memory not found: {0}")]
    NotFound(String),
    
    /// Custom error
    #[error("Custom error: {0}")]
    Custom(String),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Core error
    #[error("Core error: {0}")]
    Core(#[from] cortex_mem_core::Error),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for memory tools operations
pub type Result<T> = std::result::Result<T, ToolsError>;
