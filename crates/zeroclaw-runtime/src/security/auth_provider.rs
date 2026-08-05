//! RFC inbound authentication seam: the [`AuthProvider`] trait + a
//! default-deny [`ProviderRegistry`].
//!
//! Each provider verifies ONE credential kind (OIDC token, SSH signature, peer
//! uid, native pairing bearer) and emits a uniform
//! [`zeroclaw_api::principal::Principal`] carrying the identity / claim inputs
//! (the resolved ZeroClaw grants are added additively in the later
//! IamPolicy-wiring step, not in this slice). Dispatch,
//! audit, and per-principal isolation read that `Principal` and never see the
//! credential, so they are provider-agnostic.
//!
//! NOTE — name distinction: this `AuthProvider` (an *inbound auth* trait) is
//! unrelated to [`zeroclaw_providers::auth`]'s `AuthProvider` enum, which names
//! *outbound LLM-provider* OAuth kinds. They live in different crates and never
//! coexist in one import scope.
//!
//! This module is the foundational seam: it has no production call sites yet (the
//! registry is empty until providers are constructed at gateway/RPC boot in a
//! later phase), so it changes no runtime behaviour. Default-deny means an empty
//! registry rejects everything — wiring it on is a deliberate, later step.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::principal::{AuthMethod, AuthOutcome, DenyReason};

/// A credential presented for verification (the input to the `initialize`
/// handshake). Secret material is **redacted** in `Debug` — never log it raw.
///
/// Scoped to the accepted RFC provider set (bearer for native/OIDC, SSH
/// signature, peer uid). Not-yet-accepted credential kinds (e.g. a local
/// username/password) are added by their own scoped change, so this seam never
/// silently carries an unaccepted credential shape.
///
/// SECURITY follow-up: the secret-bearing arms are redacted in `Debug`
/// and never `Eq`-compared here, but the plaintext is not yet zeroized on drop.
/// In-memory secret scrubbing is currently absent tree-wide (even the encrypted
/// `config::secrets` store keeps plaintext un-scrubbed), so a `Zeroizing`/
/// `SecretString` convention is a separate, repo-wide hardening tracked under the
/// auth-provider work, not bolted onto this one type.
#[derive(Clone)]
#[non_exhaustive]
pub enum Credential {
    /// No credential was presented.
    None,
    /// A bearer token (native pairing token, or an OIDC access/ID token).
    Bearer(String),
    /// An SSH challenge signature over a server-issued nonce.
    SshSignature {
        username: String,
        nonce: Vec<u8>,
        signature: Vec<u8>,
    },
    /// A local transport peer credential (Unix-socket uid).
    Peercred { uid: u32 },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "Credential::None"),
            Self::Bearer(_) => write!(f, "Credential::Bearer(<redacted>)"),
            Self::SshSignature { username, .. } => f
                .debug_struct("Credential::SshSignature")
                .field("username", username)
                .field("signature", &"<redacted>")
                .finish(),
            Self::Peercred { uid } => f
                .debug_struct("Credential::Peercred")
                .field("uid", uid)
                .finish(),
        }
    }
}

/// An RFC authentication provider: verifies one credential kind and emits a
/// uniform [`AuthOutcome`]. Implementations live beside their identity source
/// (e.g. `oidc` next to the IdP introspection code, `native` over `PairingGuard`).
///
/// Fail-closed contract: `verify` returns [`AuthOutcome::Denied`] for anything it
/// cannot positively authenticate — never a silent allow.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Stable provider name = its config key (e.g. `"oidc"`, `"native"`,
    /// `"ssh-key"`). Used for enumeration and diagnostics.
    fn name(&self) -> &str;

    /// The [`AuthMethod`] this provider attests on success (also what it
    /// advertises in the handshake).
    fn method(&self) -> AuthMethod;

    /// Whether this provider can attempt the given credential kind. Lets the
    /// registry skip providers that don't apply without burning a `verify`.
    fn accepts(&self, credential: &Credential) -> bool;

    /// Verify the credential and resolve grants. Fail-closed.
    async fn verify(&self, credential: &Credential) -> AuthOutcome;
}

/// The configured set of providers, consulted in order. **Default-deny**: if no
/// provider accepts-and-authenticates the credential, the outcome is
/// [`AuthOutcome::Denied`]. An empty registry rejects everything.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn AuthProvider>>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider (boot-time wiring).
    pub fn register(&mut self, provider: Arc<dyn AuthProvider>) {
        self.providers.push(provider);
    }

    /// `true` if no provider is configured (default-deny will reject all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The methods this registry advertises (for the handshake `authMethods`).
    #[must_use]
    pub fn advertised_methods(&self) -> Vec<AuthMethod> {
        self.providers.iter().map(|p| p.method()).collect()
    }

    /// The configured provider names, in registration order — the enumeration
    /// surface exposes over RPC (no hardcoded provider lists).
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Resolve a presented credential to an [`AuthOutcome`], **default-deny and
    /// authoritative-deny**.
    ///
    /// The first accepting provider that authenticates wins. The key safety rule:
    /// a provider that *accepts* a credential but rejects it with a **specific**
    /// [`DenyReason`] (anything other than the generic [`DenyReason::BadCredential`]
    /// — e.g. [`DenyReason::MfaRequired`], [`DenyReason::TokenExpired`],
    /// [`DenyReason::Misconfigured`], [`DenyReason::AliasNotEntitled`]) is
    /// **authoritative**: that outcome is returned immediately so a later,
    /// more broadly-`accept`ing provider can NOT authenticate the same presented
    /// credential past it (e.g. an OIDC provider returning `MfaRequired` for a
    /// bearer token can't be bypassed by a later catch-all bearer provider). Only
    /// the generic `BadCredential` ("not my credential / wrong secret") lets the
    /// registry fall through to the next accepting provider. `None` is denied
    /// before any provider runs. An empty registry denies everything.
    pub async fn resolve(&self, credential: &Credential) -> AuthOutcome {
        if matches!(credential, Credential::None) {
            return AuthOutcome::Denied {
                reason: DenyReason::NoCredential,
            };
        }
        for provider in &self.providers {
            if provider.accepts(credential) {
                match provider.verify(credential).await {
                    allowed @ (AuthOutcome::Authenticated(_) | AuthOutcome::Trusted(_)) => {
                        return allowed;
                    }
                    // Specific deny = authoritative; only generic BadCredential
                    // lets a later accepting provider try the same credential.
                    AuthOutcome::Denied { reason } if reason != DenyReason::BadCredential => {
                        return AuthOutcome::Denied { reason };
                    }
                    AuthOutcome::Denied { .. } => {}
                }
            }
        }
        AuthOutcome::Denied {
            reason: DenyReason::BadCredential,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::principal::Principal;

    /// A trivial provider that accepts one fixed bearer token.
    struct FixedBearer(&'static str);

    #[async_trait]
    impl AuthProvider for FixedBearer {
        fn name(&self) -> &str {
            "fixed-bearer"
        }
        fn method(&self) -> AuthMethod {
            AuthMethod::Native
        }
        fn accepts(&self, credential: &Credential) -> bool {
            matches!(credential, Credential::Bearer(_))
        }
        async fn verify(&self, credential: &Credential) -> AuthOutcome {
            match credential {
                Credential::Bearer(t) if t == self.0 => {
                    AuthOutcome::Trusted(Principal::shared_operator())
                }
                _ => AuthOutcome::Denied {
                    reason: DenyReason::BadCredential,
                },
            }
        }
    }

    #[tokio::test]
    async fn empty_registry_is_default_deny() {
        let reg = ProviderRegistry::new();
        assert!(reg.is_empty());
        let out = reg.resolve(&Credential::Bearer("anything".into())).await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn no_credential_is_denied() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer("secret")));
        let out = reg.resolve(&Credential::None).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::NoCredential
            }
        ));
    }

    #[tokio::test]
    async fn matching_provider_authenticates() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer("secret")));
        assert_eq!(reg.advertised_methods(), vec![AuthMethod::Native]);
        assert_eq!(reg.names(), vec!["fixed-bearer"]);

        let ok = reg.resolve(&Credential::Bearer("secret".into())).await;
        assert!(ok.is_allowed());

        let bad = reg.resolve(&Credential::Bearer("wrong".into())).await;
        assert!(!bad.is_allowed());
    }

    /// A provider that accepts any bearer but always rejects with a specific reason.
    struct AlwaysMfa;

    #[async_trait]
    impl AuthProvider for AlwaysMfa {
        fn name(&self) -> &str {
            "always-mfa"
        }
        fn method(&self) -> AuthMethod {
            AuthMethod::Oidc
        }
        fn accepts(&self, credential: &Credential) -> bool {
            matches!(credential, Credential::Bearer(_))
        }
        async fn verify(&self, _credential: &Credential) -> AuthOutcome {
            AuthOutcome::Denied {
                reason: DenyReason::MfaRequired,
            }
        }
    }

    #[tokio::test]
    async fn resolve_preserves_specific_deny_reason() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(AlwaysMfa));
        // A matching provider that rejects with MfaRequired must NOT be flattened
        // to the generic BadCredential fallback.
        let out = reg.resolve(&Credential::Bearer("tok".into())).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::MfaRequired
            }
        ));
    }

    #[tokio::test]
    async fn resolve_falls_back_to_bad_credential_when_no_provider_accepts() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer("secret")));
        // No provider accepts a Peercred credential → generic BadCredential.
        let out = reg.resolve(&Credential::Peercred { uid: 1000 }).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    /// Regression (review): a provider that accepts a credential and rejects
    /// it with a SPECIFIC reason (MfaRequired) must not be bypassed by a later
    /// provider that would authenticate the same credential.
    #[tokio::test]
    async fn specific_deny_is_not_bypassed_by_a_later_provider() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(AlwaysMfa)); // accepts Bearer → MfaRequired
        reg.register(Arc::new(FixedBearer("tok"))); // would Trust Bearer("tok")
        let out = reg.resolve(&Credential::Bearer("tok".into())).await;
        assert!(
            matches!(
                out,
                AuthOutcome::Denied {
                    reason: DenyReason::MfaRequired
                }
            ),
            "a later provider must not authenticate past an authoritative MfaRequired"
        );
    }

    #[test]
    fn debug_redacts_secret_material() {
        // Bearer is fully redacted.
        assert_eq!(
            format!("{:?}", Credential::Bearer("tok".into())),
            "Credential::Bearer(<redacted>)"
        );
        // SshSignature shows the username but never the signature bytes.
        let dbg = format!(
            "{:?}",
            Credential::SshSignature {
                username: "alice".into(),
                nonce: vec![1, 2, 3],
                signature: vec![0xde, 0xad, 0xbe, 0xef],
            }
        );
        assert!(dbg.contains("alice"));
        assert!(dbg.contains("<redacted>"));
        assert!(!dbg.contains("222")); // 0xde — raw signature byte must not appear
    }
}
