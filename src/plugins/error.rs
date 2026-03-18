//! Plugin error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("failed to load WASM module: {0}")]
    LoadFailed(String),

    #[error("plugin execution failed: {0}")]
    ExecutionFailed(String),

    #[error("permission denied: plugin '{plugin}' requires '{permission}'")]
    PermissionDenied { plugin: String, permission: String },

    #[error("plugin '{0}' is already loaded")]
    AlreadyLoaded(String),

    #[error("plugin capability not supported: {0}")]
    UnsupportedCapability(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
}
