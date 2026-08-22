//! The `native` provider: authenticates the existing gateway pairing
//! bearer token as the shared operator, over the ONE live pairing
//! authority.
//!
//! The wrapped [`PairingGuard`] is the same instance the gateway uses for
//! `/pair`, rotation, and revocation (its token set is shared interior
//! state), so pairing a new device or revoking a token affects RPC
//! authentication immediately — there is no boot-time token snapshot.
//! Verification uses the guard's strict membership check: an empty token
//! set denies everything regardless of the gateway's `require_pairing`
//! convenience setting.
//!
//! A pairing token attests "trusted operator", not a distinct per-user
//! identity, so success maps to the shared-operator sentinel.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::principal::{AuthMethod, AuthOutcome, AuthenticatedIdentity, DenyReason};
use zeroclaw_config::pairing::PairingGuard;

use super::{AuthProvider, Credential};

pub struct NativeAuthProvider {
    guard: Arc<PairingGuard>,
}

impl NativeAuthProvider {
    /// Wrap the daemon's canonical live pairing authority. Callers must
    /// pass the SAME guard instance the gateway serves `/pair` and
    /// revocation from — constructing a second guard from a config
    /// snapshot would fork the authority.
    #[must_use]
    pub fn new(guard: Arc<PairingGuard>) -> Self {
        Self { guard }
    }

    /// The live authority, for connection-scoped liveness re-checks
    /// (an established connection stores only the token's SHA-256 hash
    /// and consults this guard before privileged operations).
    #[must_use]
    pub fn guard(&self) -> &Arc<PairingGuard> {
        &self.guard
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
            Credential::Bearer(token) if self.guard.token_is_paired(token) => {
                AuthOutcome::Verified(AuthenticatedIdentity::shared_operator(AuthMethod::Native))
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
    use zeroclaw_api::principal::{IdentitySubject, PrincipalId};

    fn provider_with(tokens: &[&str]) -> NativeAuthProvider {
        let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
        NativeAuthProvider::new(Arc::new(PairingGuard::new(true, &tokens)))
    }

    #[tokio::test]
    async fn valid_paired_token_verifies_as_the_shared_operator() {
        let provider = provider_with(&["zc_valid_token"]);
        let out = provider
            .verify(&Credential::Bearer("zc_valid_token".into()))
            .await;
        let identity = out.identity().expect("verified");
        assert_eq!(identity.subject, IdentitySubject::SharedOperator);
        assert_eq!(identity.method, AuthMethod::Native);
        assert_eq!(
            identity.subject.principal_id().as_str(),
            PrincipalId::SHARED_OPERATOR,
            "a pairing token attests the shared operator, not a distinct user"
        );
    }

    #[tokio::test]
    async fn wrong_token_and_empty_set_are_denied() {
        let provider = provider_with(&["zc_valid_token"]);
        assert!(
            !provider
                .verify(&Credential::Bearer("zc_wrong".into()))
                .await
                .is_allowed()
        );
        let empty = provider_with(&[]);
        assert!(
            !empty
                .verify(&Credential::Bearer("anything".into()))
                .await
                .is_allowed(),
            "an empty token set fails closed"
        );
    }

    #[tokio::test]
    async fn revocation_on_the_shared_guard_applies_live() {
        // The RFC's live-authority requirement: revoking a token on the
        // guard the gateway serves invalidates it here with no reload.
        let guard = Arc::new(PairingGuard::new(true, &["zc_tok".to_string()]));
        let provider = NativeAuthProvider::new(Arc::clone(&guard));
        assert!(
            provider
                .verify(&Credential::Bearer("zc_tok".into()))
                .await
                .is_allowed()
        );
        assert!(guard.revoke_token("zc_tok"));
        assert!(
            !provider
                .verify(&Credential::Bearer("zc_tok".into()))
                .await
                .is_allowed(),
            "revocation must deny before the next verification"
        );
    }

    #[tokio::test]
    async fn pairing_on_the_shared_guard_applies_live() {
        let guard = Arc::new(PairingGuard::new(true, &[]));
        let provider = NativeAuthProvider::new(Arc::clone(&guard));
        assert!(
            !provider
                .verify(&Credential::Bearer("zc_new".into()))
                .await
                .is_allowed()
        );
        let code = guard.pairing_code().expect("fresh guard mints a code");
        let token = guard
            .try_pair(&code, "test-client")
            .await
            .expect("no lockout")
            .expect("code accepted");
        assert!(
            provider
                .verify(&Credential::Bearer(token))
                .await
                .is_allowed(),
            "a token paired through the gateway flow authenticates immediately"
        );
    }

    #[tokio::test]
    async fn hashed_token_form_is_accepted_on_load() {
        let hash = PairingGuard::token_hash("zc_valid_token");
        let provider = provider_with(&[hash.as_str()]);
        assert!(
            provider
                .verify(&Credential::Bearer("zc_valid_token".into()))
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn non_bearer_credentials_are_not_accepted() {
        let provider = provider_with(&["zc_valid_token"]);
        assert!(!provider.accepts(&Credential::Peercred { uid: 1000 }));
        assert!(!provider.accepts(&Credential::None));
        assert!(
            !provider
                .verify(&Credential::Peercred { uid: 1000 })
                .await
                .is_allowed()
        );
    }
}
