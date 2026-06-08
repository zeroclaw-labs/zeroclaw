//! DaemonClaw identity layer — `LocalIdentityProvider` and the factory.
//!
//! This crate is the always-on default identity backend. It generates or
//! loads an Ed25519 keypair, persists it encrypted at rest via
//! `SecretStore`, and exposes a stable `AgentIdentity` view + assertion
//! signing + local signature verification. There is no network — the
//! remote-issuer (WardToken) path lives in the `daemonclaw-wardtoken`
//! crate.
//!
//! ## Crate layout
//!
//! - [`spki`] — RFC 8410 §3.3 SPKI prefix + WardToken `fingerprint_der`
//!   computation. Cross-system contract; pinned by known-vector test.
//! - [`canonical`] — deterministic byte serialization of `IdentityAssertion`
//!   for signing. Pin for the wire format.
//! - [`state`] — encrypted on-disk state file. Mode 0600, atomic rename,
//!   operator-readable SPKI PEM sibling file.
//! - [`local`] — `LocalIdentityProvider` impl, keypair lifecycle, sign/verify.
//! - [`runtime`] — `IdentityRuntimeOptions` (factory input).
//! - [`error`] — `IdentityError` + `IdentityResult`.
//!
//! ## Factory
//!
//! [`create_identity_provider`] mirrors the LLM provider factory in shape:
//! a string match on the configured provider name. An unknown name is an
//! error, not a graceful degradation — misconfiguration is loud.

pub mod canonical;
pub mod error;
pub mod local;
pub mod runtime;
pub mod secret_store;
pub mod spki;
pub mod state;

use daemonclaw_api::identity::IdentityProvider;

pub use canonical::{canonical_bytes, CanonicalAssertion};
pub use error::{IdentityError, IdentityResult};
pub use local::{
    sign_canonical, verify_canonical, LocalIdentityProvider,
};
pub use runtime::IdentityRuntimeOptions;
pub use spki::{fingerprint_pubkey, fingerprint_spki, spki_from_pubkey, spki_pem_to_der, ED25519_SPKI_PREFIX};

/// Factory: build the right identity provider from a string key.
///
/// Currently only `"local"` is supported. `daemonclaw-wardtoken` adds
/// `"wardtoken"` and registers its own factory; the runtime boot
/// sequence picks the right one based on `[identity_provider]`.
pub fn create_identity_provider(
    name: &str,
    options: IdentityRuntimeOptions,
) -> IdentityResult<Box<dyn IdentityProvider>> {
    match name {
        "local" => Ok(Box::new(LocalIdentityProvider::new(options)?)),
        other => Err(IdentityError::UnknownProvider(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_dispatches_local() {
        let opts = IdentityRuntimeOptions::default();
        let provider = create_identity_provider("local", opts).unwrap();
        assert_eq!(provider.name(), "local");
    }

    #[test]
    fn factory_rejects_unknown_provider() {
        let opts = IdentityRuntimeOptions::default();
        let result = create_identity_provider("nope", opts);
        let err = result.err().expect("expected error for unknown provider");
        let msg = err.to_string();
        assert!(msg.contains("unknown identity provider"), "got: {msg}");
        assert!(msg.contains("nope"), "got: {msg}");
    }
}
