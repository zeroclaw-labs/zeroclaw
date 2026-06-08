//! Error types for the identity layer.

use thiserror::Error;

use crate::secret_store;

/// Errors raised by identity providers and the factory.
#[derive(Debug, Error)]
pub enum IdentityError {
    /// The configured `provider` name is not recognized.
    #[error("unknown identity provider: {0}")]
    UnknownProvider(String),

    /// Ed25519 key generation or PKCS#8 parsing failed.
    #[error("key material error: {0}")]
    KeyMaterial(String),

    /// The SecretStore returned an error during encrypt/decrypt.
    #[error("secret store error: {0}")]
    SecretStore(#[from] secret_store::SecretStoreError),

    /// The on-disk state file is corrupt or in an unexpected format.
    #[error("identity state error: {0}")]
    State(String),

    /// I/O error reading or writing identity artifacts.
    #[error("identity I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Canonical serialization or signature operation failed.
    #[error("cryptographic operation failed: {0}")]
    Crypto(String),
}

/// Convenience alias used throughout the crate.
pub type IdentityResult<T> = Result<T, IdentityError>;
