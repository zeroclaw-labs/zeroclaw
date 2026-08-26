//! RFC 7141 inbound authentication seam: the [`AuthProvider`] trait + a
//! default-deny [`ProviderRegistry`] with EXPLICIT provider selection.
//!
//! Each provider verifies ONE credential kind (OIDC token, peer uid, native
//! pairing bearer, …) and emits a uniform
//! [`zeroclaw_api::principal::AuthenticatedIdentity`] — identity and evidence
//! only. Grants are resolved separately by the shared principal resolver from
//! the current permission-profile configuration, so providers cannot carry an
//! authorization vocabulary and cannot snapshot policy.
//!
//! Credential routing is explicit and unambiguous (Rev 8): the handshake
//! names the intended provider ([`ProviderRegistry::resolve_named`]) unless
//! the credential format securely identifies it
//! ([`ProviderRegistry::route_transport`] for kernel-supplied peer
//! credentials). A credential is never offered to every configured provider,
//! and once a provider was selected its denial is authoritative — there is no
//! fallback to another provider or to broader authority.
//!
//! NOTE — name distinction: this `AuthProvider` (an *inbound auth* trait) is
//! unrelated to [`zeroclaw_providers::auth`]'s `AuthProvider` enum, which names
//! *outbound LLM-provider* OAuth kinds. They live in different crates and never
//! coexist in one import scope.
//!
//! This module is the foundational seam: it has no production call sites yet
//! (the registry is empty until providers are constructed at gateway/RPC boot
//! in a later phase), so it changes no runtime behaviour. Default-deny means
//! an empty registry rejects everything — wiring it on is a deliberate, later
//! step.

use std::collections::HashMap;
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
    /// A bearer token (native pairing token, or an OIDC access token).
    Bearer(String),
    /// An SSH challenge signature over a server-issued nonce.
    SshSignature {
        username: String,
        nonce: Vec<u8>,
        signature: Vec<u8>,
    },
    /// A local transport peer credential (Unix-socket uid). Kernel-supplied:
    /// the transport, not the client, constructs this arm.
    Peercred { uid: u32 },
}

impl Credential {
    /// Whether the credential's FORMAT securely identifies its provider
    /// class without the client naming one: today only the kernel-supplied
    /// peer credential qualifies. A bearer string identifies nothing by
    /// itself — routing it requires an explicit provider selection.
    #[must_use]
    pub fn is_transport_intrinsic(&self) -> bool {
        matches!(self, Self::Peercred { .. })
    }
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
/// (e.g. `oidc` next to the token-verification code, `native` over
/// `PairingGuard`).
///
/// Fail-closed contract: `verify` returns [`AuthOutcome::Denied`] for anything
/// it cannot positively authenticate — never a silent allow. Providers verify
/// credentials into identities; they never resolve or attach grants.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Stable provider name = its config selection key (e.g. `"native"`,
    /// `"peercred"`, `"oidc.corp"` for `[oidc.corp]`). Unique within a
    /// registry; the handshake selects a provider by this name.
    fn name(&self) -> &str;

    /// The [`AuthMethod`] this provider attests on success (also what it
    /// advertises in the handshake).
    fn method(&self) -> AuthMethod;

    /// Whether this provider can attempt the given credential kind. Used to
    /// reject a mis-kinded credential before `verify`, and to route
    /// transport-intrinsic credentials — never to try one credential across
    /// providers.
    fn accepts(&self, credential: &Credential) -> bool;

    /// Verify the credential into an identity. Fail-closed.
    async fn verify(&self, credential: &Credential) -> AuthOutcome;
}

/// The configured set of providers, selected by name. **Default-deny**: an
/// empty registry rejects everything, an unknown selection rejects, a
/// mis-kinded credential rejects, and a selected provider's denial is final.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn AuthProvider>>,
    by_name: HashMap<String, usize>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider (boot-time wiring). Provider names are the
    /// selection keys, so a duplicate is ambiguous routing — refused rather
    /// than silently shadowed.
    pub fn register(&mut self, provider: Arc<dyn AuthProvider>) -> anyhow::Result<()> {
        let name = provider.name().to_owned();
        if self.by_name.contains_key(&name) {
            anyhow::bail!("auth provider name {name:?} is already registered");
        }
        self.by_name.insert(name, self.providers.len());
        self.providers.push(provider);
        Ok(())
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
    /// surface exposed over RPC (no hardcoded provider lists). These are the
    /// valid `resolve_named` selections.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.name()).collect()
    }

    /// Verify `credential` against the EXPLICITLY selected provider.
    ///
    /// Fail-closed and authoritative: an unknown name, a missing credential,
    /// or a credential kind the selected provider does not accept is denied;
    /// and whatever the selected provider decides is final — a denial can
    /// never fall through to another provider that would have accepted the
    /// same credential (Rev 8: "failure is authoritative once a method and
    /// provider have been selected").
    pub async fn resolve_named(&self, name: &str, credential: &Credential) -> AuthOutcome {
        if matches!(credential, Credential::None) {
            return AuthOutcome::Denied {
                reason: DenyReason::NoCredential,
            };
        }
        let Some(provider) = self.by_name.get(name).map(|i| &self.providers[*i]) else {
            return AuthOutcome::Denied {
                reason: DenyReason::UnknownProvider,
            };
        };
        if !provider.accepts(credential) {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        }
        bind_provenance(provider.as_ref(), provider.verify(credential).await)
    }

    /// Route a transport-intrinsic credential (today: the kernel-supplied
    /// peer uid) to the ONE provider that handles its kind.
    ///
    /// This is not a fallback scan: a credential whose format does not
    /// securely identify its provider (any bearer, any signature) is denied
    /// here — it must go through [`Self::resolve_named`]. Zero accepting
    /// providers deny; more than one accepting provider is ambiguous wiring
    /// and denies as misconfigured rather than picking silently.
    pub async fn route_transport(&self, credential: &Credential) -> AuthOutcome {
        if matches!(credential, Credential::None) {
            return AuthOutcome::Denied {
                reason: DenyReason::NoCredential,
            };
        }
        if !credential.is_transport_intrinsic() {
            return AuthOutcome::Denied {
                reason: DenyReason::UnknownProvider,
            };
        }
        let mut accepting = self.providers.iter().filter(|p| p.accepts(credential));
        let Some(provider) = accepting.next() else {
            return AuthOutcome::Denied {
                reason: DenyReason::UnknownProvider,
            };
        };
        if accepting.next().is_some() {
            return AuthOutcome::Denied {
                reason: DenyReason::Misconfigured,
            };
        }
        bind_provenance(provider.as_ref(), provider.verify(credential).await)
    }
}

/// Bind the selected provider to the identity it returns: reject a `Verified`
/// outcome whose method, subject class, or provider alias does not match the
/// provider the handshake selected. RFC 7141's trusted-provenance boundary must
/// not depend on every provider implementation being correct — a miswired
/// provider registered under one name cannot borrow another method, another
/// issuer's profile mapping, or the shared-operator sentinel.
fn bind_provenance(provider: &dyn AuthProvider, outcome: AuthOutcome) -> AuthOutcome {
    use zeroclaw_api::principal::{AuthMethod, IdentitySubject};
    let AuthOutcome::Verified(identity) = &outcome else {
        return outcome;
    };
    let method_ok = identity.method == provider.method();
    let subject_ok = matches!(
        (identity.method, &identity.subject),
        (AuthMethod::Native, IdentitySubject::SharedOperator)
            | (AuthMethod::Peercred, IdentitySubject::SharedOperator)
            | (AuthMethod::Peercred, IdentitySubject::Roster { .. })
            | (AuthMethod::Oidc, IdentitySubject::Oidc { .. })
            | (AuthMethod::Oidc, IdentitySubject::Service { .. })
            | (AuthMethod::SharedOperator, IdentitySubject::SharedOperator)
    );
    // An `oidc.<alias>` provider must return that same alias: the resolver
    // selects the issuer/profile mapping from `provider_alias`, so a mismatch
    // could borrow a different issuer's mapping.
    let alias_ok = match provider.name().strip_prefix("oidc.") {
        Some(expected) => identity.provider_alias.as_deref() == Some(expected),
        None => true,
    };
    if method_ok && subject_ok && alias_ok {
        outcome
    } else {
        AuthOutcome::Denied {
            reason: DenyReason::Misconfigured,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::principal::{AuthenticatedIdentity, IdentitySubject};

    /// A trivial provider that accepts one fixed bearer token.
    struct FixedBearer {
        name: &'static str,
        token: &'static str,
    }

    #[async_trait]
    impl AuthProvider for FixedBearer {
        fn name(&self) -> &str {
            self.name
        }
        fn method(&self) -> AuthMethod {
            AuthMethod::Native
        }
        fn accepts(&self, credential: &Credential) -> bool {
            matches!(credential, Credential::Bearer(_))
        }
        async fn verify(&self, credential: &Credential) -> AuthOutcome {
            match credential {
                Credential::Bearer(t) if t == self.token => AuthOutcome::Verified(
                    AuthenticatedIdentity::shared_operator(AuthMethod::Native),
                ),
                _ => AuthOutcome::Denied {
                    reason: DenyReason::BadCredential,
                },
            }
        }
    }

    /// A provider that accepts any bearer but always rejects with a specific
    /// reason.
    struct AlwaysMfa;

    #[async_trait]
    impl AuthProvider for AlwaysMfa {
        fn name(&self) -> &str {
            "oidc.corp"
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

    /// A trivial peercred provider mapping one uid to a roster identity.
    struct FixedPeercred {
        name: &'static str,
        uid: u32,
    }

    #[async_trait]
    impl AuthProvider for FixedPeercred {
        fn name(&self) -> &str {
            self.name
        }
        fn method(&self) -> AuthMethod {
            AuthMethod::Peercred
        }
        fn accepts(&self, credential: &Credential) -> bool {
            matches!(credential, Credential::Peercred { .. })
        }
        async fn verify(&self, credential: &Credential) -> AuthOutcome {
            match credential {
                Credential::Peercred { uid } if *uid == self.uid => {
                    AuthOutcome::Verified(AuthenticatedIdentity::new(
                        IdentitySubject::Roster {
                            principal_id: "bob".into(),
                        },
                        AuthMethod::Peercred,
                    ))
                }
                _ => AuthOutcome::Denied {
                    reason: DenyReason::BadCredential,
                },
            }
        }
    }

    /// A miswired provider whose verify() returns an identity that does not
    /// match its declared provenance (method / subject class / alias). Used to
    /// prove `bind_provenance` rejects such a Verified outcome.
    struct Miswired {
        bad: AuthenticatedIdentity,
    }

    #[async_trait]
    impl AuthProvider for Miswired {
        fn name(&self) -> &str {
            "oidc.corp"
        }
        fn method(&self) -> AuthMethod {
            AuthMethod::Oidc
        }
        fn accepts(&self, credential: &Credential) -> bool {
            matches!(credential, Credential::Bearer(_))
        }
        async fn verify(&self, _credential: &Credential) -> AuthOutcome {
            AuthOutcome::Verified(self.bad.clone())
        }
    }

    async fn miswired_is_denied(bad: AuthenticatedIdentity) {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(Miswired { bad })).unwrap();
        let out = reg.resolve_named("oidc.corp", &bearer("x")).await;
        assert!(
            matches!(
                out,
                AuthOutcome::Denied {
                    reason: DenyReason::Misconfigured
                }
            ),
            "bind_provenance must reject a mismatched identity"
        );
    }

    #[tokio::test]
    async fn provenance_rejects_method_mismatch() {
        // oidc.corp returns a Native-method identity.
        miswired_is_denied(
            AuthenticatedIdentity::new(
                IdentitySubject::Oidc {
                    issuer: "https://sso".into(),
                    subject: "s".into(),
                },
                AuthMethod::Native,
            )
            .with_provider_alias("corp"),
        )
        .await;
    }

    #[tokio::test]
    async fn provenance_rejects_shared_operator_from_oidc_provider() {
        // A non-native provider returning the shared-operator sentinel.
        miswired_is_denied(
            AuthenticatedIdentity::shared_operator(AuthMethod::Oidc).with_provider_alias("corp"),
        )
        .await;
    }

    #[tokio::test]
    async fn provenance_rejects_oidc_alias_mismatch() {
        // oidc.corp returns an identity claiming a different alias, which the
        // resolver would use to pick another issuer's mapping.
        miswired_is_denied(
            AuthenticatedIdentity::new(
                IdentitySubject::Oidc {
                    issuer: "https://sso".into(),
                    subject: "s".into(),
                },
                AuthMethod::Oidc,
            )
            .with_provider_alias("other"),
        )
        .await;
    }

    fn bearer(token: &str) -> Credential {
        Credential::Bearer(token.into())
    }

    #[tokio::test]
    async fn empty_registry_is_default_deny() {
        let reg = ProviderRegistry::new();
        assert!(reg.is_empty());
        let out = reg.resolve_named("native", &bearer("anything")).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::UnknownProvider
            }
        ));
        let routed = reg
            .route_transport(&Credential::Peercred { uid: 1000 })
            .await;
        assert!(!routed.is_allowed());
    }

    #[tokio::test]
    async fn no_credential_is_denied() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "secret",
        }))
        .unwrap();
        let out = reg.resolve_named("native", &Credential::None).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::NoCredential
            }
        ));
        let routed = reg.route_transport(&Credential::None).await;
        assert!(matches!(
            routed,
            AuthOutcome::Denied {
                reason: DenyReason::NoCredential
            }
        ));
    }

    #[tokio::test]
    async fn named_selection_verifies_and_enumerates() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "secret",
        }))
        .unwrap();
        assert_eq!(reg.advertised_methods(), vec![AuthMethod::Native]);
        assert_eq!(reg.names(), vec!["native"]);

        let ok = reg.resolve_named("native", &bearer("secret")).await;
        assert!(ok.is_allowed());

        let bad = reg.resolve_named("native", &bearer("wrong")).await;
        assert!(!bad.is_allowed());

        let unknown = reg.resolve_named("oidc.corp", &bearer("secret")).await;
        assert!(matches!(
            unknown,
            AuthOutcome::Denied {
                reason: DenyReason::UnknownProvider
            }
        ));
    }

    #[tokio::test]
    async fn selected_provider_denial_is_authoritative_no_fallback() {
        // Rev 8: once a provider is selected, its denial is final. A second
        // registered provider that WOULD accept the same bearer must never be
        // consulted.
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(AlwaysMfa)).unwrap();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "tok",
        }))
        .unwrap();
        let out = reg.resolve_named("oidc.corp", &bearer("tok")).await;
        assert!(
            matches!(
                out,
                AuthOutcome::Denied {
                    reason: DenyReason::MfaRequired
                }
            ),
            "the same credential must not be re-tried against the native provider"
        );
    }

    #[tokio::test]
    async fn mis_kinded_credential_for_selected_provider_is_denied() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "secret",
        }))
        .unwrap();
        let out = reg
            .resolve_named("native", &Credential::Peercred { uid: 1000 })
            .await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn duplicate_provider_name_is_refused() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "a",
        }))
        .unwrap();
        let err = reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "b",
        }));
        assert!(
            err.is_err(),
            "duplicate selection keys are ambiguous routing"
        );
    }

    #[tokio::test]
    async fn transport_routing_reaches_the_single_peercred_provider() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "secret",
        }))
        .unwrap();
        reg.register(Arc::new(FixedPeercred {
            name: "peercred",
            uid: 1000,
        }))
        .unwrap();
        let ok = reg
            .route_transport(&Credential::Peercred { uid: 1000 })
            .await;
        assert!(ok.is_allowed());
        let bad = reg.route_transport(&Credential::Peercred { uid: 1 }).await;
        assert!(!bad.is_allowed());
    }

    #[tokio::test]
    async fn transport_routing_never_scans_bearers() {
        // A bearer format identifies no provider: routing it without an
        // explicit selection would be credential spraying.
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedBearer {
            name: "native",
            token: "secret",
        }))
        .unwrap();
        let out = reg.route_transport(&bearer("secret")).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::UnknownProvider
            }
        ));
    }

    #[tokio::test]
    async fn ambiguous_transport_routing_fails_closed() {
        let mut reg = ProviderRegistry::new();
        reg.register(Arc::new(FixedPeercred {
            name: "peercred",
            uid: 1000,
        }))
        .unwrap();
        reg.register(Arc::new(FixedPeercred {
            name: "peercred-2",
            uid: 1000,
        }))
        .unwrap();
        let out = reg
            .route_transport(&Credential::Peercred { uid: 1000 })
            .await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::Misconfigured
            }
        ));
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
