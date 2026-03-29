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

    #[error("plugin '{0}' is unsigned and signature verification is required")]
    UnsignedPlugin(String),

    #[error("plugin '{plugin}' signed by untrusted publisher key '{publisher_key}'")]
    UntrustedPublisher {
        plugin: String,
        publisher_key: String,
    },

    #[error("invalid plugin signature: {0}")]
    SignatureInvalid(String),

    #[error("missing required field '{field}' in plugin manifest")]
    MissingField { field: String },

    #[error("malformed TOML in plugin manifest: {0}")]
    MalformedToml(String),

    #[error("plugin '{plugin}' is missing required config keys: {keys}")]
    MissingConfig { plugin: String, keys: String },

    #[error("plugin '{plugin}' declares wildcard host '{host}' which is forbidden at {level} security level")]
    WildcardHostRejected {
        plugin: String,
        host: String,
        level: String,
    },

    #[error("plugin '{plugin}' declares forbidden path '{path}' in allowed_paths")]
    ForbiddenPath { plugin: String, path: String },

    #[error("plugin '{plugin}' declares path '{path}' outside workspace root '{workspace}' (strict mode)")]
    PathOutsideWorkspace {
        plugin: String,
        path: String,
        workspace: String,
    },

    #[error("plugin '{plugin}' is not allowlisted in paranoid mode")]
    PluginNotAllowlisted { plugin: String },

    #[error("failed to decrypt config key '{key}': {source}")]
    ConfigDecrypt {
        key: String,
        source: anyhow::Error,
    },

    #[error("WASM binary integrity check failed for plugin '{plugin}': expected hash {expected}, got {actual}")]
    HashMismatch {
        plugin: String,
        expected: String,
        actual: String,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
}
