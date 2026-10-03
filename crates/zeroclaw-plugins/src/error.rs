//! Plugin error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin not found: {0}")]
    NotFound(String),

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    /// Filesystem identity changed; never evidence of defective package contents.
    #[error("plugin namespace changed: {0}")]
    NamespaceChanged(String),

    /// A package operation could not finish and kept the files it held at
    /// `path`: for example a claimed package it could not restore without
    /// replacing another occupant, or a staged package it could not publish.
    /// `reason` says why.
    #[error("plugin recovery retained bytes at {path}: {reason}")]
    RecoveryRetained { path: String, reason: String },

    #[error("invalid plugin config: {0}")]
    InvalidConfig(String),

    #[error("invalid plugin instance identity: {0}")]
    InvalidInstanceId(String),

    #[error("invalid plugin endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("failed to load WASM module: {0}")]
    LoadFailed(String),

    #[error("plugin execution failed: {0}")]
    ExecutionFailed(String),

    #[error("permission denied: plugin '{plugin}' is not authorized for '{permission}'")]
    PermissionDenied { plugin: String, permission: String },

    #[error("plugin '{0}' is already loaded")]
    AlreadyLoaded(String),

    /// Something occupies a package name in the plugins directory that the host
    /// has not admitted as that package, and it was left as found. Install
    /// never overwrites it. `remove` deletes an unloaded directory only when it
    /// is empty, or when admission rejects its own contents rather than its
    /// signature. `reason` says why it was kept.
    #[error("'{name}' in the plugins directory was left untouched: {reason}")]
    UnadmittedPackage { name: String, reason: String },

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

    #[error("plugin '{0}' must declare signed wasm_sha256 in strict mode")]
    PayloadDigestRequired(String),

    #[error("invalid WASM payload SHA-256: {0}")]
    PayloadDigestInvalid(String),

    #[error("WASM payload digest mismatch (expected {expected}, got {actual})")]
    PayloadDigestMismatch { expected: String, actual: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
}
