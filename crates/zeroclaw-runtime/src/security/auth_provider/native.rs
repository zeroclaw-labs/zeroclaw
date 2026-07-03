//! RFC #7141 `native` provider: wraps the existing [`PairingGuard`] bearer
//! token so single-operator deployments keep a working auth path with no IdP.
//!
//! Semantics are exactly today's gateway pairing: the presented bearer is
//! SHA-256 hashed and compared against the paired-token set. Success maps to
//! [`AuthOutcome::Trusted`] with the shared-operator sentinel because a pairing
//! token attests "trusted operator", not a distinct per-user identity.

use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::principal::{AuthMethod, AuthOutcome, DenyReason, Principal};

use super::{AuthProvider, Credential};
use crate::security::pairing::PairingGuard;

pub struct NativeAuthProvider {
    guard: Arc<PairingGuard>,
}

impl NativeAuthProvider {
    /// Wrap an existing pairing guard. The guard should be constructed with
    /// `require_pairing = true` for auth use: a guard with pairing disabled
    /// accepts every token, which is only correct on surfaces that already
    /// treat the transport itself as trusted.
    #[must_use]
    pub fn new(guard: Arc<PairingGuard>) -> Self {
        Self { guard }
    }

    /// Build a guard over the persisted gateway token set, always requiring
    /// pairing. An empty token set therefore denies everything (fail closed)
    /// instead of falling open.
    #[must_use]
    pub fn from_paired_tokens(paired_tokens: &[String]) -> Self {
        Self {
            guard: Arc::new(PairingGuard::new(true, paired_tokens)),
        }
    }
}

#[async_trait]
impl AuthProvider for NativeAuthProvider {
    fn name(&self) -> &str {
        "native"
    }

    fn method(&self) -> AuthMethod {
        AuthMethod::Native
    }

    fn accepts(&self, credential: &Credential) -> bool {
        matches!(credential, Credential::Bearer(_))
    }

    async fn verify(&self, credential: &Credential) -> AuthOutcome {
        match credential {
            Credential::Bearer(token) if self.guard.is_authenticated(token) => {
                let mut principal = Principal::shared_operator();
                principal.auth_method = AuthMethod::Native;
                AuthOutcome::Trusted(principal)
            }
            _ => AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn valid_paired_token_is_trusted() {
        let provider = NativeAuthProvider::from_paired_tokens(&["zc_valid_token".to_string()]);
        let out = provider
            .verify(&Credential::Bearer("zc_valid_token".into()))
            .await;
        assert!(out.is_allowed());
        let p = out.principal().expect("principal bound");
        assert_eq!(p.auth_method, AuthMethod::Native);
        assert_eq!(
            p.id.as_str(),
            zeroclaw_api::principal::PrincipalId::SHARED_OPERATOR,
            "native token attests the shared operator, not a distinct user"
        );
    }

    #[tokio::test]
    async fn wrong_token_is_denied() {
        let provider = NativeAuthProvider::from_paired_tokens(&["zc_valid_token".to_string()]);
        let out = provider
            .verify(&Credential::Bearer("zc_wrong".into()))
            .await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn empty_token_set_fails_closed() {
        let provider = NativeAuthProvider::from_paired_tokens(&[]);
        let out = provider
            .verify(&Credential::Bearer("anything".into()))
            .await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn non_bearer_credentials_are_not_accepted() {
        let provider = NativeAuthProvider::from_paired_tokens(&["zc_valid_token".to_string()]);
        assert!(!provider.accepts(&Credential::Peercred { uid: 1000 }));
        assert!(!provider.accepts(&Credential::None));
        let out = provider.verify(&Credential::Peercred { uid: 1000 }).await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn hashed_token_form_is_accepted_on_load() {
        let hash = PairingGuard::token_hash("zc_valid_token");
        let provider = NativeAuthProvider::from_paired_tokens(&[hash]);
        let out = provider
            .verify(&Credential::Bearer("zc_valid_token".into()))
            .await;
        assert!(out.is_allowed());
    }
}
