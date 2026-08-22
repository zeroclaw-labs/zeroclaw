//! RPC inbound authentication: the daemon-side bundle of the provider
//! registry, the shared principal resolver, and the live local-identity
//! bindings, consumed by the `initialize` handshake and the per-method
//! authorization gate.
//!
//! Credential routing (RFC 7141 Rev 8) is explicit:
//! - an `auth_token` in the handshake selects the provider named by
//!   `auth_provider`, defaulting to `native` (the pairing token) when
//!   unnamed — a fixed selection, never a scan across providers, and the
//!   selected provider's denial is final;
//! - with no token, a kernel-supplied peer credential routes to the one
//!   peercred provider;
//! - with neither, only a LOCAL connection with no `[users]` roster
//!   configured keeps the legacy trusted path (the socket mode / pipe ACL
//!   is the credential). Once a roster exists, or on any remote
//!   connection, no credential means denial — never shared-operator
//!   fallback.
//!
//! The compiled policy is generation-stamped: `refresh_from_config` swaps
//! the resolver policy and the uid roster together, so profile, mapping,
//! and roster edits reach established connections at their next privileged
//! operation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, FORBIDDEN};
use zeroclaw_api::principal::{
    AuthMethod, AuthOutcome, AuthenticatedIdentity, DenyReason, Principal,
};
use zeroclaw_config::pairing::PairingGuard;
use zeroclaw_config::schema::Config;

use super::transport::TransportKind;
use crate::security::auth_provider::{
    Credential, NativeAuthProvider, OidcAuthProvider, PeercredAuthProvider, ProviderRegistry,
    UidRoster,
};
use crate::security::principal_resolver::PrincipalResolver;

/// The authenticated state one connection holds after `initialize`.
/// Grants are a stamped resolution, not a snapshot: the gate re-resolves
/// from `identity` whenever the authorization generation moves.
#[derive(Clone, Debug)]
pub struct ConnectionAuth {
    /// The provider-verified identity (retained so grants can be
    /// re-resolved after a policy change; claims are non-secret).
    pub identity: AuthenticatedIdentity,
    /// The canonical resolved principal.
    pub principal: Principal,
    /// Effective grants at `generation`.
    pub grants: ResolvedGrants,
    /// The authorization-policy generation `grants` was resolved at.
    pub generation: u64,
    /// SHA-256 of the native pairing bearer, when this connection
    /// authenticated with one — non-secret evidence for live revocation
    /// checks against the pairing authority. Never the bearer itself.
    pub native_token_hash: Option<String>,
}

/// A handshake or authorization denial, pre-mapped to its JSON-RPC error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthDenied {
    pub code: i32,
    pub message: String,
}

impl AuthDenied {
    pub(crate) fn auth_required(message: impl Into<String>) -> Self {
        Self {
            code: AUTH_REQUIRED,
            message: message.into(),
        }
    }

    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self {
            code: FORBIDDEN,
            message: message.into(),
        }
    }

    pub(crate) fn from_deny_reason(reason: DenyReason) -> Self {
        match reason {
            DenyReason::NoCredential => Self::auth_required(
                "Authentication required: present auth_token in initialize, or connect \
                 from a mapped local uid",
            ),
            DenyReason::BadCredential => Self::auth_required("Credential rejected"),
            DenyReason::TokenExpired => {
                Self::auth_required("Credential expired: re-initialize with a fresh token")
            }
            DenyReason::MfaRequired => {
                Self::auth_required("Authentication assurance not met (MFA/ACR required)")
            }
            DenyReason::UnknownProvider => Self::auth_required("Unknown auth_provider selection"),
            DenyReason::NotEntitled => Self::forbidden(
                "Authenticated, but no permission profile grants this identity anything",
            ),
            DenyReason::AliasNotEntitled => {
                Self::forbidden("Principal is not entitled to the requested agent")
            }
            DenyReason::Misconfigured => {
                Self::forbidden("Authentication is misconfigured on this daemon (fail closed)")
            }
            // DenyReason is non_exhaustive; anything unknown fails closed.
            _ => Self::auth_required("Credential rejected"),
        }
    }
}

/// The daemon's inbound-auth layer: providers, resolver, and live local
/// bindings. One instance per daemon generation, shared by every
/// connection.
pub struct RpcInboundAuth {
    registry: ProviderRegistry,
    resolver: PrincipalResolver,
    uid_roster: Arc<UidRoster>,
    pairing: Arc<PairingGuard>,
    /// Whether any `[users]` roster entry exists in the CURRENT policy —
    /// once true, the no-credential local compatibility path is closed.
    local_roster_configured: AtomicBool,
    /// Live view of `security.trust_daemon_uid`, shared with the peercred
    /// provider so narrowing it applies without restart.
    trust_daemon_uid: Arc<AtomicBool>,
}

impl RpcInboundAuth {
    /// Build from a validated config and the daemon's canonical live
    /// pairing authority (the same instance the gateway serves `/pair`
    /// and revocation from).
    pub fn from_config(config: &Config, pairing: Arc<PairingGuard>) -> anyhow::Result<Self> {
        let uid_roster = Arc::new(UidRoster::from_config(config));
        let trust_daemon_uid = Arc::new(AtomicBool::new(config.security.trust_daemon_uid));
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(NativeAuthProvider::new(Arc::clone(&pairing))))?;
        registry.register(Arc::new(PeercredAuthProvider::new(
            PeercredAuthProvider::current_process_uid(),
            Arc::clone(&trust_daemon_uid),
            Arc::clone(&uid_roster),
        )))?;
        let mut aliases: Vec<&String> = config.oidc.keys().collect();
        aliases.sort();
        for alias in aliases {
            registry.register(Arc::new(OidcAuthProvider::new(
                alias.clone(),
                config.oidc[alias].clone(),
            )?))?;
        }
        let auth = Self {
            registry,
            resolver: PrincipalResolver::from_config(config),
            uid_roster,
            pairing,
            local_roster_configured: AtomicBool::new(!config.users.is_empty()),
            trust_daemon_uid,
        };
        Ok(auth)
    }

    /// Test-only permissive layer: empty auth config, fresh pairing guard.
    /// Local connections resolve through the legacy shared-operator path.
    pub fn for_tests(config: &Config) -> Arc<Self> {
        let pairing = Arc::new(PairingGuard::new(
            config.gateway.require_pairing,
            &config.gateway.paired_tokens,
        ));
        Arc::new(Self::from_config(config, pairing).expect("test auth config is valid"))
    }

    /// The shared resolver (generation source).
    pub fn resolver(&self) -> &PrincipalResolver {
        &self.resolver
    }

    /// The live pairing authority, for per-operation revocation checks.
    pub fn pairing(&self) -> &Arc<PairingGuard> {
        &self.pairing
    }

    /// The handshake's advertised provider names.
    pub fn provider_names(&self) -> Vec<String> {
        self.registry
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Re-compile policy from the current config: resolver policy, uid
    /// roster, and the roster-configured flag move together, and every
    /// previously stamped generation becomes stale. Returns the new
    /// generation.
    pub fn refresh_from_config(&self, config: &Config) -> u64 {
        self.uid_roster.replace_from_config(config);
        self.local_roster_configured
            .store(!config.users.is_empty(), Ordering::Release);
        self.trust_daemon_uid
            .store(config.security.trust_daemon_uid, Ordering::Release);
        self.resolver.replace_from_config(config)
    }

    /// Authenticate one `initialize` handshake into a [`ConnectionAuth`].
    pub async fn authenticate(
        &self,
        transport: TransportKind,
        transport_credential: Credential,
        auth_token: Option<&str>,
        auth_provider: Option<&str>,
    ) -> Result<ConnectionAuth, AuthDenied> {
        let (outcome, native_token_hash) = if let Some(token) = auth_token {
            // Explicit credential wins over the transport-intrinsic one.
            // Unnamed bearers select the native pairing provider — a fixed
            // default, not a scan.
            let selection = auth_provider.unwrap_or("native");
            let credential = Credential::Bearer(token.to_owned());
            let hash = (selection == "native").then(|| PairingGuard::token_hash(token));
            (
                self.registry.resolve_named(selection, &credential).await,
                hash,
            )
        } else if transport_credential.is_transport_intrinsic() {
            (
                self.registry.route_transport(&transport_credential).await,
                None,
            )
        } else {
            match transport {
                // Local compatibility (RFC 7141 §migration): with no local
                // roster configured, the socket mode / pipe ACL remains
                // the credential and the connection is the shared
                // operator. The moment a roster exists this path closes —
                // a failed or absent credential never falls back.
                TransportKind::Local if !self.local_roster_configured.load(Ordering::Acquire) => (
                    AuthOutcome::Verified(AuthenticatedIdentity::shared_operator(
                        AuthMethod::SharedOperator,
                    )),
                    None,
                ),
                TransportKind::Local => {
                    return Err(AuthDenied::auth_required(
                        "A local user roster is configured: connect from a mapped uid or \
                         present auth_token in initialize",
                    ));
                }
                TransportKind::Wss => {
                    return Err(AuthDenied::auth_required(
                        "Remote connections must present auth_token in initialize",
                    ));
                }
            }
        };

        let identity = match outcome {
            AuthOutcome::Verified(identity) => identity,
            AuthOutcome::Denied { reason } => return Err(AuthDenied::from_deny_reason(reason)),
        };
        let resolved = self
            .resolver
            .resolve(&identity)
            .map_err(AuthDenied::from_deny_reason)?;
        Ok(ConnectionAuth {
            identity,
            principal: resolved.principal,
            grants: resolved.grants,
            generation: resolved.generation,
            native_token_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_api::principal::{ActorKind, PrincipalId};
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    fn base_config() -> Config {
        Config::default()
    }

    fn config_with_roster(uid: u32) -> Config {
        let mut config = base_config();
        config.permission_profiles.insert(
            "operator".into(),
            PermissionProfileConfig {
                grants: std::collections::HashMap::from([(Resource::Sessions, vec![Verb::Read])]),
                ..PermissionProfileConfig::default()
            },
        );
        config.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(uid),
                permission_profiles: vec!["operator".into()],
            },
        );
        config
    }

    fn auth_for(config: &Config, tokens: &[&str]) -> RpcInboundAuth {
        let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
        RpcInboundAuth::from_config(config, Arc::new(PairingGuard::new(true, &tokens)))
            .expect("valid")
    }

    #[tokio::test]
    async fn local_with_no_roster_keeps_the_legacy_trusted_path() {
        let auth = auth_for(&base_config(), &[]);
        let conn = auth
            .authenticate(TransportKind::Local, Credential::None, None, None)
            .await
            .expect("legacy local path");
        assert_eq!(conn.principal.id.as_str(), PrincipalId::SHARED_OPERATOR);
        assert!(conn.grants.admin, "single-operator behavior is unchanged");
    }

    #[tokio::test]
    async fn local_with_a_roster_closes_the_no_credential_path() {
        let auth = auth_for(&config_with_roster(4242), &[]);
        let denied = auth
            .authenticate(TransportKind::Local, Credential::None, None, None)
            .await
            .unwrap_err();
        assert_eq!(denied.code, AUTH_REQUIRED);
    }

    #[tokio::test]
    async fn remote_without_a_token_is_denied() {
        let auth = auth_for(&base_config(), &["zc_tok"]);
        let denied = auth
            .authenticate(TransportKind::Wss, Credential::None, None, None)
            .await
            .unwrap_err();
        assert_eq!(denied.code, AUTH_REQUIRED);
    }

    #[tokio::test]
    async fn remote_pairing_token_authenticates_and_records_liveness_hash() {
        let auth = auth_for(&base_config(), &["zc_tok"]);
        let conn = auth
            .authenticate(TransportKind::Wss, Credential::None, Some("zc_tok"), None)
            .await
            .expect("paired token authenticates");
        assert_eq!(conn.principal.id.as_str(), PrincipalId::SHARED_OPERATOR);
        assert_eq!(
            conn.native_token_hash.as_deref(),
            Some(PairingGuard::token_hash("zc_tok").as_str()),
            "the connection retains only the hash, for live revocation checks"
        );
        assert!(
            auth.pairing()
                .token_hash_is_paired(conn.native_token_hash.as_deref().unwrap())
        );
    }

    #[tokio::test]
    async fn wrong_token_is_denied_with_no_fallback() {
        let auth = auth_for(&base_config(), &["zc_tok"]);
        let denied = auth
            .authenticate(TransportKind::Wss, Credential::None, Some("zc_wrong"), None)
            .await
            .unwrap_err();
        assert_eq!(denied.code, AUTH_REQUIRED);
    }

    #[tokio::test]
    async fn unknown_provider_selection_is_denied() {
        let auth = auth_for(&base_config(), &["zc_tok"]);
        let denied = auth
            .authenticate(
                TransportKind::Wss,
                Credential::None,
                Some("zc_tok"),
                Some("oidc.ghost"),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code, AUTH_REQUIRED);
    }

    #[tokio::test]
    async fn peercred_routes_to_the_roster_principal() {
        let auth = auth_for(&config_with_roster(4242), &[]);
        let conn = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: 4242 },
                None,
                None,
            )
            .await
            .expect("roster uid authenticates");
        assert_eq!(conn.principal.id.as_str(), "user:alice");
        assert_eq!(conn.principal.actor, ActorKind::Human);
        assert!(conn.grants.permits(Resource::Sessions, Verb::Read));
        assert!(!conn.grants.admin);
    }

    #[tokio::test]
    async fn unmatched_peercred_is_denied_even_with_no_roster() {
        // daemon uid is current process uid; an arbitrary other uid with
        // no roster entry must not reach the legacy path — the provider's
        // denial is authoritative.
        let auth = auth_for(&base_config(), &[]);
        let foreign_uid = PeercredAuthProvider::current_process_uid().wrapping_add(1);
        let denied = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: foreign_uid },
                None,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code, AUTH_REQUIRED);
    }

    #[tokio::test]
    async fn refresh_bumps_generation_and_rebinds_the_roster() {
        let auth = auth_for(&config_with_roster(4242), &[]);
        let before = auth.resolver().generation();
        // Roster entry removed: local no-credential path stays CLOSED?
        // No — with the roster gone the compatibility path reopens, and
        // the previously mapped uid loses its principal.
        let generation = auth.refresh_from_config(&base_config());
        assert!(generation > before);
        let denied = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: 4242 },
                None,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            denied.code, AUTH_REQUIRED,
            "unbound uid denies after refresh"
        );
    }

    #[tokio::test]
    async fn startup_with_wss_and_a_pairing_path_constructs() {
        let mut config = base_config();
        config.wss.enabled = true;
        config.gateway.require_pairing = true;
        assert!(
            RpcInboundAuth::from_config(&config, Arc::new(PairingGuard::new(true, &[]))).is_ok(),
            "pairing-capable WSS config is startable; handshakes deny until paired"
        );
    }
}
