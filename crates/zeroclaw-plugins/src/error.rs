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

    #[error("domain must have at least two labels (e.g. 'example.com'): {0}")]
    AddressStringTooShort(String),

    #[error("invalid DNS label (empty, too long, or illegal characters): {0}")]
    AddressStringInvalidLabel(String),

    #[error("wildcard '*' is only allowed at subdomain level 3 or higher (not in TLD or SLD): {0}")]
    AddressStringWildcardNotAllowed(String),

    #[error("address is not a valid IPv4, IPv6, or domain name: {0}")]
    AddressStringInvalidAddress(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// Errors originating from the wasmtime runtime (Component Model path).
    ///
    /// `wasmtime::Error` uses its own internal error type rather than
    /// `std::error::Error` directly, so we capture the message string here
    /// to stay compatible with the `thiserror`/`anyhow` error hierarchy.
    #[cfg(feature = "plugins-wasmtime")]
    #[error("wasmtime error: {0}")]
    Wasmtime(String),
}

/// Convert a `wasmtime::Error` into a [`PluginError::Wasmtime`].
///
/// This explicit impl (rather than `#[from]`) avoids requiring
/// `wasmtime::Error: std::error::Error` — wasmtime's error type uses
/// `core::error::Error` internally and the two impls are the same on
/// Rust ≥ 1.81, but an explicit conversion is clearer at call sites.
#[cfg(feature = "plugins-wasmtime")]
impl From<wasmtime::Error> for PluginError {
    fn from(e: wasmtime::Error) -> Self {
        PluginError::Wasmtime(format!("{e:#}"))
    }
}
