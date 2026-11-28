//! The `native` provider: authenticates the existing gateway pairing
//! bearer token over the ONE live pairing authority.
//!
//! The wrapped [`PairingGuard`] is the same instance the gateway uses for
//! `/pair`, rotation, and revocation (its token set is shared interior
//! state), so pairing a new device or revoking a token affects RPC
//! authentication immediately — there is no boot-time token snapshot.
//! Verification uses the guard's strict membership check: an empty token
//! set denies everything regardless of the gateway's `require_pairing`
//! convenience setting.
//!
//! An unbound pairing token attests "trusted operator", not a distinct
//! per-user identity, so it maps to the shared-operator sentinel. A token
//! the operator bound to a `[users.<name>]` entry when minting its code
//! maps to that roster principal instead, and the shared resolver grants it
//! only that user's permission profiles.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::principal::{
    AuthMethod, AuthOutcome, AuthenticatedIdentity, DenyReason, IdentitySubject,
};
use zeroclaw_config::pairing::{PairedTokenSubject, PairingGuard};

/// The identity a paired token attests. The provider, connection
/// revalidation and the gateway's pairing responses all derive native
/// identities here, so none of them can disagree with the handshake.
#[must_use]
pub fn identity_for_subject(subject: PairedTokenSubject) -> AuthenticatedIdentity {
    match subject {
        PairedTokenSubject::SharedOperator => {
            AuthenticatedIdentity::shared_operator(AuthMethod::Native)
        }
        PairedTokenSubject::RosterUser { principal_id } => {
            AuthenticatedIdentity::new(IdentitySubject::Roster { principal_id }, AuthMethod::Native)
        }
    }
}

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
            Credential::Bearer(token) => match self.guard.subject_for_token(token) {
                Some(subject) => AuthOutcome::Verified(identity_for_subject(subject)),
                None => AuthOutcome::Denied {
                    reason: DenyReason::BadCredential,
                },
            },
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
    use zeroclaw_config::pairing::PairingCodePolicy;

    fn provider_with(tokens: &[&str]) -> NativeAuthProvider {
        let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
        NativeAuthProvider::new(Arc::new(PairingGuard::new(
            true,
            &tokens,
            PairingCodePolicy::default(),
        )))
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

    fn bound_provider() -> (Arc<PairingGuard>, NativeAuthProvider) {
        let guard = Arc::new(PairingGuard::from_gateway_config(
            &zeroclaw_config::schema::GatewayConfig {
                paired_tokens: vec!["zc_shared".into()],
                paired_token_users: std::collections::HashMap::from([(
                    PairingGuard::token_hash("zc_bound"),
                    "alice".to_string(),
                )]),
                ..zeroclaw_config::schema::GatewayConfig::default()
            },
        ));
        (Arc::clone(&guard), NativeAuthProvider::new(guard))
    }

    #[tokio::test]
    async fn bound_token_verifies_as_its_roster_principal() {
        let (_, provider) = bound_provider();
        let out = provider
            .verify(&Credential::Bearer("zc_bound".into()))
            .await;
        let identity = out.identity().expect("verified");
        assert_eq!(
            identity.subject,
            IdentitySubject::Roster {
                principal_id: "alice".into()
            }
        );
        assert_eq!(identity.method, AuthMethod::Native);
        assert_eq!(identity.subject.principal_id().as_str(), "user:alice");
    }

    #[tokio::test]
    async fn unbound_token_beside_a_bound_one_stays_the_shared_operator() {
        let (_, provider) = bound_provider();
        let out = provider
            .verify(&Credential::Bearer("zc_shared".into()))
            .await;
        assert_eq!(
            out.identity().expect("verified").subject,
            IdentitySubject::SharedOperator
        );
    }

    #[tokio::test]
    async fn revoking_a_bound_token_applies_live() {
        let (guard, provider) = bound_provider();
        assert!(guard.revoke_token("zc_bound"));
        assert!(
            !provider
                .verify(&Credential::Bearer("zc_bound".into()))
                .await
                .is_allowed(),
            "a revoked bound token must not verify as anything"
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
        let guard = Arc::new(PairingGuard::new(
            true,
            &["zc_tok".to_string()],
            PairingCodePolicy::default(),
        ));
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
        let guard = Arc::new(PairingGuard::new(true, &[], PairingCodePolicy::default()));
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
