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

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use parking_lot::RwLock;

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
use crate::security::principal_resolver::{PrincipalResolver, ResolvedPrincipal, ResolverPolicy};

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
    /// Non-secret evidence that can be rechecked after a policy generation
    /// changes. Bearers themselves never outlive initialize.
    pub local_evidence: LocalCredentialEvidence,
}

/// Only non-secret, local authentication evidence retained on a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalCredentialEvidence {
    /// Live pairing membership is rechecked by the retained SHA-256 hash.
    NativeTokenHash,
    /// The kernel-supplied uid is reclassified against the accepted roster.
    Peercred { uid: u32 },
    /// Legacy local shared-operator mode remains valid only while no roster
    /// exists in the accepted policy.
    LocalCompatibility,
    /// An OIDC verifier policy change invalidates the connection: the bearer
    /// is deliberately not retained, so the client must initialize again.
    Oidc,
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
            DenyReason::NoCredential => Self::auth_required(crate::i18n::get_required_cli_string(
                "rpc-auth-required-token",
            )),
            DenyReason::BadCredential => Self::auth_required(crate::i18n::get_required_cli_string(
                "rpc-auth-credential-rejected",
            )),
            DenyReason::TokenExpired => Self::auth_required(crate::i18n::get_required_cli_string(
                "rpc-auth-credential-expired",
            )),
            DenyReason::MfaRequired => Self::auth_required(crate::i18n::get_required_cli_string(
                "rpc-auth-assurance-required",
            )),
            DenyReason::UnknownProvider => Self::auth_required(
                crate::i18n::get_required_cli_string("rpc-auth-unknown-provider"),
            ),
            DenyReason::NotEntitled => Self::forbidden(crate::i18n::get_required_cli_string(
                "rpc-auth-not-entitled",
            )),
            DenyReason::AliasNotEntitled => Self::forbidden(crate::i18n::get_required_cli_string(
                "rpc-auth-alias-not-entitled",
            )),
            DenyReason::Misconfigured => Self::forbidden(crate::i18n::get_required_cli_string(
                "rpc-auth-misconfigured",
            )),
            // DenyReason is non_exhaustive; anything unknown fails closed.
            _ => Self::auth_required(crate::i18n::get_required_cli_string(
                "rpc-auth-credential-rejected",
            )),
        }
    }
}

/// The daemon's inbound-auth layer: providers, resolver, and live local
/// bindings. One instance per daemon generation, shared by every
/// connection.
///
/// Published as one unit by [`RpcInboundAuth::refresh_from_config`] and
/// [`RpcInboundAuth::publish_accepted`]; its fields stay private so a
/// consumer can only observe the snapshot through the accessors that keep
/// registry, resolver, roster and flags consistent.
pub struct AcceptedAuthState {
    registry: ProviderRegistry,
    resolver: PrincipalResolver,
    uid_roster: Arc<UidRoster>,
    local_roster_configured: bool,
    trust_daemon_uid: Arc<AtomicBool>,
    daemon_uid: u32,
    deny_all: bool,
    /// The configuration this state was compiled from (see [`auth_inputs`]),
    /// or `None` for the deny-all state, which was compiled from nothing.
    auth_inputs: Option<serde_json::Value>,
}

/// The parts of a configuration an accepted authorization state is compiled
/// from: the OIDC, roster, and permission-profile sections and the daemon-uid
/// trust posture. The pairing authority is shared live state, not config.
fn auth_inputs(config: &Config) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::Value::Array(vec![
        serde_json::to_value(&config.oidc)?,
        serde_json::to_value(&config.users)?,
        serde_json::to_value(&config.permission_profiles)?,
        serde_json::Value::Bool(config.security.trust_daemon_uid),
    ]))
}

impl AcceptedAuthState {
    fn from_config(
        config: &Config,
        pairing: Arc<PairingGuard>,
        generation: u64,
    ) -> anyhow::Result<Self> {
        let uid_roster = Arc::new(UidRoster::from_config(config));
        let trust_daemon_uid = Arc::new(AtomicBool::new(config.security.trust_daemon_uid));
        let daemon_uid = PeercredAuthProvider::current_process_uid();
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(NativeAuthProvider::new(Arc::clone(&pairing))))?;
        registry.register(Arc::new(PeercredAuthProvider::new(
            daemon_uid,
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
        Ok(Self {
            registry,
            resolver: PrincipalResolver::with_generation(
                ResolverPolicy::from_config(config)?,
                generation,
            ),
            uid_roster,
            local_roster_configured: !config.users.is_empty(),
            trust_daemon_uid,
            daemon_uid,
            deny_all: false,
            auth_inputs: Some(auth_inputs(config)?),
        })
    }

    fn deny_all(config: &Config, pairing: Arc<PairingGuard>, generation: u64) -> Self {
        let daemon_uid = PeercredAuthProvider::current_process_uid();
        let uid_roster = Arc::new(UidRoster::from_config(config));
        let trust_daemon_uid = Arc::new(AtomicBool::new(false));
        let mut registry = ProviderRegistry::new();
        // These providers keep the handshake surface stable while resolution
        // below denies every identity until a valid auth policy is accepted.
        registry
            .register(Arc::new(NativeAuthProvider::new(pairing)))
            .expect("unique native provider");
        registry
            .register(Arc::new(PeercredAuthProvider::new(
                daemon_uid,
                Arc::clone(&trust_daemon_uid),
                Arc::clone(&uid_roster),
            )))
            .expect("unique peercred provider");
        Self {
            registry,
            resolver: PrincipalResolver::with_generation(ResolverPolicy::default(), generation),
            uid_roster,
            local_roster_configured: true,
            trust_daemon_uid,
            daemon_uid,
            deny_all: true,
            auth_inputs: None,
        }
    }

    fn resolve(&self, identity: &AuthenticatedIdentity) -> Result<ResolvedPrincipal, DenyReason> {
        if self.deny_all {
            return Err(DenyReason::NotEntitled);
        }
        self.resolver.resolve(identity)
    }

    fn revalidates_local_evidence(
        &self,
        identity: &AuthenticatedIdentity,
        evidence: &LocalCredentialEvidence,
        native_token_hash: Option<&str>,
        pairing: &PairingGuard,
    ) -> Result<(), DenyReason> {
        let reverified = match evidence {
            LocalCredentialEvidence::NativeTokenHash => native_token_hash
                .is_some_and(|hash| pairing.token_hash_is_paired(hash))
                .then(|| AuthenticatedIdentity::shared_operator(AuthMethod::Native)),
            LocalCredentialEvidence::Peercred { uid }
                if *uid == self.daemon_uid && self.trust_daemon_uid.load(Ordering::Relaxed) =>
            {
                Some(AuthenticatedIdentity::shared_operator(AuthMethod::Peercred))
            }
            LocalCredentialEvidence::Peercred { uid } => {
                self.uid_roster.principal_id_for(*uid).map(|principal_id| {
                    AuthenticatedIdentity::new(
                        zeroclaw_api::principal::IdentitySubject::Roster { principal_id },
                        AuthMethod::Peercred,
                    )
                })
            }
            LocalCredentialEvidence::LocalCompatibility if !self.local_roster_configured => Some(
                AuthenticatedIdentity::shared_operator(AuthMethod::SharedOperator),
            ),
            // A changed OIDC verifier must not keep accepting an identity
            // verified under a prior policy; the bearer is not retained.
            LocalCredentialEvidence::Oidc => None,
            _ => None,
        };
        match reverified {
            Some(reverified)
                if reverified.subject == identity.subject
                    && reverified.method == identity.method =>
            {
                Ok(())
            }
            _ => Err(DenyReason::BadCredential),
        }
    }
}

/// Prove that an authorization policy compiles, without a live auth layer in
/// hand.
///
/// [`RpcInboundAuth::validate_refresh_from_config`] is the same test for a
/// caller that owns the live layer. Surfaces that stage a configuration
/// without one (the gateway's Quickstart) use this so they can refuse an
/// invalid policy before their first persistent write rather than after it.
/// Pairing state does not affect whether the policy compiles.
pub fn validate_accepted_auth_config(config: &Config) -> anyhow::Result<()> {
    let pairing = Arc::new(PairingGuard::new(
        config.gateway.require_pairing,
        &config.gateway.paired_tokens,
        config.gateway.pairing_code,
    ));
    let _ = AcceptedAuthState::from_config(config, pairing, 1)?;
    Ok(())
}

/// The daemon's inbound-auth layer. The accepted state is a single snapshot:
/// registry, resolver/generation, uid roster, and local trust posture are
/// rebuilt and published together or not at all.
pub struct RpcInboundAuth {
    state: RwLock<Arc<AcceptedAuthState>>,
    pairing: Arc<PairingGuard>,
    /// The persistence revision the accepted state was built from.
    ///
    /// The generation counts policy installations; this counts accepted
    /// persistences of the configuration those installations came from. A
    /// consumer that has just persisted revision N can wait for the accepted
    /// state to reach N rather than guessing from the generation, and a
    /// publication carrying an older revision is refused, so a slow writer
    /// cannot reinstall superseded policy.
    accepted_revision: AtomicU64,
}

impl RpcInboundAuth {
    pub fn from_config(config: &Config, pairing: Arc<PairingGuard>) -> anyhow::Result<Self> {
        let state = match AcceptedAuthState::from_config(config, Arc::clone(&pairing), 1) {
            Ok(state) => state,
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "error": format!("{error}") })),
                    "Authorization config is invalid; installing a deny-all policy until it is repaired and reloaded"
                );
                AcceptedAuthState::deny_all(config, Arc::clone(&pairing), 1)
            }
        };
        Ok(Self {
            state: RwLock::new(Arc::new(state)),
            pairing,
            accepted_revision: AtomicU64::new(0),
        })
    }

    /// Test-only permissive layer: empty auth config, fresh pairing guard.
    /// Local connections resolve through the legacy shared-operator path.
    pub fn for_tests(config: &Config) -> Arc<Self> {
        let pairing = Arc::new(PairingGuard::new(
            config.gateway.require_pairing,
            &config.gateway.paired_tokens,
            config.gateway.pairing_code,
        ));
        Arc::new(Self::from_config(config, pairing).expect("test auth config is valid"))
    }

    fn state(&self) -> Arc<AcceptedAuthState> {
        Arc::clone(&self.state.read())
    }

    /// Current authorization generation from the accepted snapshot.
    pub fn generation(&self) -> u64 {
        self.state().resolver.generation()
    }

    /// Resolve against one coherent accepted snapshot.
    pub fn resolve(
        &self,
        identity: &AuthenticatedIdentity,
    ) -> Result<ResolvedPrincipal, DenyReason> {
        self.state().resolve(identity)
    }

    /// The live pairing authority, for per-operation revocation checks.
    pub fn pairing(&self) -> &Arc<PairingGuard> {
        &self.pairing
    }

    /// The handshake's advertised provider names.
    pub fn provider_names(&self) -> Vec<String> {
        self.state()
            .registry
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    /// Re-compile policy from the current config: resolver policy, uid
    /// roster, and the roster-configured flag move together, and every
    /// previously stamped generation becomes stale. Returns the new
    /// generation.
    ///
    /// The resolver validates and installs FIRST: if the auth sections are
    /// invalid it returns the error and nothing here changes — the previous
    /// policy, roster, flags, and generation all stay in effect.
    pub fn refresh_from_config(&self, config: &Config) -> anyhow::Result<u64> {
        let mut slot = self.state.write();
        let generation = slot.resolver.generation().saturating_add(1);
        let next = AcceptedAuthState::from_config(config, Arc::clone(&self.pairing), generation)?;
        *slot = Arc::new(next);
        Ok(generation)
    }

    /// The revision of the configuration the accepted state was built from.
    pub fn accepted_revision(&self) -> u64 {
        self.accepted_revision.load(Ordering::Acquire)
    }

    /// Publish an accepted policy built from a configuration that has been
    /// persisted as `revision`.
    ///
    /// Takes the same write lock over the accepted state that
    /// [`Self::refresh_from_config`] takes, so the state and the revision it
    /// carries are installed together. A `revision` that is not newer than
    /// the accepted one is refused as a no-op and the current generation is
    /// returned, which is what keeps a writer that was slow to publish from
    /// reinstalling superseded policy over a newer one.
    ///
    /// A persistence that leaves every authorization input as it was (see
    /// `auth_inputs`) records its revision without moving the generation.
    /// Nothing the accepted state is compiled from changed, and moving the
    /// generation would make every established binding re-resolve over an
    /// unrelated edit, which an OIDC binding cannot survive.
    ///
    /// Returns the generation in force after the call.
    pub fn publish_accepted(&self, config: &Config, revision: u64) -> anyhow::Result<u64> {
        let mut slot = self.state.write();
        if revision <= self.accepted_revision.load(Ordering::Acquire) {
            return Ok(slot.resolver.generation());
        }
        config.validate_auth()?;
        if slot.auth_inputs.as_ref() == Some(&auth_inputs(config)?) {
            self.accepted_revision.store(revision, Ordering::Release);
            return Ok(slot.resolver.generation());
        }
        let generation = slot.resolver.generation().saturating_add(1);
        let next = AcceptedAuthState::from_config(config, Arc::clone(&self.pairing), generation)?;
        *slot = Arc::new(next);
        self.accepted_revision.store(revision, Ordering::Release);
        Ok(generation)
    }

    /// The accepted state, but only once it has caught up to `revision`.
    ///
    /// Fail-closed: a consumer that needs to act on a policy it just
    /// persisted gets a denial rather than the previous state when the
    /// publication has not landed yet.
    pub fn accepted_at_least(&self, revision: u64) -> Result<Arc<AcceptedAuthState>, DenyReason> {
        let state = self.state();
        if self.accepted_revision.load(Ordering::Acquire) >= revision {
            Ok(state)
        } else {
            Err(DenyReason::Misconfigured)
        }
    }

    /// Prove that an auth snapshot can be compiled before a caller persists
    /// a config edit. The caller still uses [`Self::refresh_from_config`] to
    /// publish it after the save boundary.
    pub fn validate_refresh_from_config(&self, config: &Config) -> anyhow::Result<()> {
        let generation = self.generation().saturating_add(1);
        let _ = AcceptedAuthState::from_config(config, Arc::clone(&self.pairing), generation)?;
        Ok(())
    }

    /// Recheck non-secret local evidence and resolve one newly accepted
    /// generation. OIDC changes require initialize because this stage never
    /// retains the bearer that could be reverified.
    pub fn revalidate_and_resolve(
        &self,
        auth: &ConnectionAuth,
    ) -> Result<ResolvedPrincipal, DenyReason> {
        let state = self.state();
        state.revalidates_local_evidence(
            &auth.identity,
            &auth.local_evidence,
            auth.native_token_hash.as_deref(),
            &self.pairing,
        )?;
        state.resolve(&auth.identity)
    }

    /// Resolve an established binding against the accepted policy, rechecking
    /// its local credential evidence only when its stamped generation is no
    /// longer current.
    ///
    /// At an unchanged generation that evidence cannot have been invalidated:
    /// the OIDC verifiers, the uid roster, and the daemon-uid trust posture
    /// change only through a new publication, and the caller checks expiry,
    /// the revalidation deadline, and pairing liveness on every operation.
    /// This is the rule the dispatcher's gate applies, and it is what keeps an
    /// OIDC binding, whose bearer is never retained, usable until a
    /// publication changes the authorization inputs; a config save that
    /// leaves them unchanged does not move the generation. After such a
    /// change, [`Self::revalidate_and_resolve`] applies and an OIDC binding
    /// must initialize again.
    pub fn resolve_current(&self, auth: &ConnectionAuth) -> Result<ResolvedPrincipal, DenyReason> {
        let state = self.state();
        if auth.generation != state.resolver.generation() {
            state.revalidates_local_evidence(
                &auth.identity,
                &auth.local_evidence,
                auth.native_token_hash.as_deref(),
                &self.pairing,
            )?;
        }
        state.resolve(&auth.identity)
    }

    /// Authenticate one `initialize` handshake into a [`ConnectionAuth`].
    pub async fn authenticate(
        &self,
        transport: TransportKind,
        transport_credential: Credential,
        auth_token: Option<&str>,
        auth_provider: Option<&str>,
    ) -> Result<ConnectionAuth, AuthDenied> {
        let state = self.state();
        let (outcome, native_token_hash, local_evidence) = if let Some(token) = auth_token {
            // Explicit credential wins over the transport-intrinsic one.
            // Unnamed bearers select the native pairing provider — a fixed
            // default, not a scan.
            let selection = auth_provider.unwrap_or("native");
            let credential = Credential::Bearer(token.to_owned());
            let hash = (selection == "native").then(|| PairingGuard::token_hash(token));
            (
                state.registry.resolve_named(selection, &credential).await,
                hash,
                if selection == "native" {
                    LocalCredentialEvidence::NativeTokenHash
                } else {
                    LocalCredentialEvidence::Oidc
                },
            )
        } else if transport_credential.is_transport_intrinsic() {
            (
                state.registry.route_transport(&transport_credential).await,
                None,
                match transport_credential {
                    Credential::Peercred { uid } => LocalCredentialEvidence::Peercred { uid },
                    _ => unreachable!(),
                },
            )
        } else {
            match transport {
                // Local compatibility (RFC 7141 §migration): with no local
                // roster configured, the socket mode / pipe ACL remains
                // the credential and the connection is the shared
                // operator. The moment a roster exists this path closes —
                // a failed or absent credential never falls back.
                TransportKind::Local if !state.local_roster_configured => (
                    AuthOutcome::Verified(AuthenticatedIdentity::shared_operator(
                        AuthMethod::SharedOperator,
                    )),
                    None,
                    LocalCredentialEvidence::LocalCompatibility,
                ),
                TransportKind::Local => {
                    return Err(AuthDenied::auth_required(
                        crate::i18n::get_required_cli_string("rpc-auth-local-roster-required"),
                    ));
                }
                TransportKind::Wss => {
                    return Err(AuthDenied::auth_required(
                        crate::i18n::get_required_cli_string("rpc-auth-remote-token-required"),
                    ));
                }
            }
        };

        let identity = match outcome {
            AuthOutcome::Verified(identity) => identity,
            AuthOutcome::Denied { reason } => return Err(AuthDenied::from_deny_reason(reason)),
        };
        let resolved = state
            .resolve(&identity)
            .map_err(AuthDenied::from_deny_reason)?;
        Ok(ConnectionAuth {
            identity,
            principal: resolved.principal,
            grants: resolved.grants,
            generation: resolved.generation,
            native_token_hash,
            local_evidence,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::grants::{Resource, Verb};
    use zeroclaw_api::principal::{ActorKind, PrincipalId};
    use zeroclaw_config::pairing::PairingCodePolicy;
    use zeroclaw_config::schema::{OidcConfig, PermissionProfileConfig, UserConfig};

    fn base_config() -> Config {
        Config::default()
    }

    fn shared_operator_identity() -> AuthenticatedIdentity {
        AuthenticatedIdentity::shared_operator(AuthMethod::SharedOperator)
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
        RpcInboundAuth::from_config(
            config,
            Arc::new(PairingGuard::new(
                true,
                &tokens,
                PairingCodePolicy::default(),
            )),
        )
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
        let before = auth.generation();
        // Roster entry removed: local no-credential path stays CLOSED?
        // No — with the roster gone the compatibility path reopens, and
        // the previously mapped uid loses its principal.
        let generation = auth
            .refresh_from_config(&base_config())
            .expect("a valid base config refreshes");
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
    async fn rejected_refresh_preserves_the_complete_accepted_snapshot() {
        let config = config_with_roster(4242);
        let auth = auth_for(&config, &[]);
        let before_generation = auth.generation();
        let before_names = auth.provider_names();

        let mut rejected = config;
        rejected.users.clear();
        rejected.security.trust_daemon_uid = false;
        rejected.oidc.insert("broken".into(), OidcConfig::default());
        assert!(auth.refresh_from_config(&rejected).is_err());
        assert_eq!(auth.generation(), before_generation);
        assert_eq!(auth.provider_names(), before_names);
        assert!(
            auth.authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: 4242 },
                None,
                None,
            )
            .await
            .is_ok(),
            "a rejected candidate must not leak its roster or trust changes"
        );
    }

    #[test]
    fn refresh_rebuilds_the_live_oidc_provider_registry() {
        let auth = auth_for(&config_with_roster(4242), &[]);
        assert!(!auth.provider_names().iter().any(|name| name == "oidc.corp"));

        let mut with_oidc = config_with_roster(4242);
        with_oidc.oidc.insert(
            "corp".into(),
            OidcConfig {
                issuer: "https://sso.example.com".into(),
                audience: "zeroclaw".into(),
                claim_path: "groups".into(),
                profile_map: std::collections::HashMap::from([("ops".into(), "operator".into())]),
                ..OidcConfig::default()
            },
        );
        auth.refresh_from_config(&with_oidc)
            .expect("valid OIDC config refreshes");
        assert!(auth.provider_names().iter().any(|name| name == "oidc.corp"));

        auth.refresh_from_config(&config_with_roster(4242))
            .expect("valid OIDC removal refreshes");
        assert!(!auth.provider_names().iter().any(|name| name == "oidc.corp"));
    }

    #[tokio::test]
    async fn invalid_startup_denies_shared_operator_routes() {
        let mut invalid = base_config();
        invalid.oidc.insert("broken".into(), OidcConfig::default());
        let auth = auth_for(&invalid, &["zc_tok"]);
        for (transport, token) in [
            (TransportKind::Local, None),
            (TransportKind::Wss, Some("zc_tok")),
        ] {
            auth.authenticate(transport, Credential::None, token, None)
                .await
                .expect_err("invalid startup policy must deny every principal");
        }
    }

    #[tokio::test]
    async fn stale_peercred_is_reverified_before_grants_are_reused() {
        let config = config_with_roster(4242);
        let auth = auth_for(&config, &[]);
        let conn = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: 4242 },
                None,
                None,
            )
            .await
            .expect("initial peer credential");
        auth.refresh_from_config(&base_config())
            .expect("valid replacement policy");
        assert!(
            auth.revalidate_and_resolve(&conn).is_err(),
            "a removed uid cannot retain old grants through a stale connection"
        );
    }

    #[tokio::test]
    async fn startup_with_wss_and_a_pairing_path_constructs() {
        let mut config = base_config();
        config.wss.enabled = true;
        config.gateway.require_pairing = true;
        assert!(
            RpcInboundAuth::from_config(
                &config,
                Arc::new(PairingGuard::new(true, &[], PairingCodePolicy::default()))
            )
            .is_ok(),
            "pairing-capable WSS config is startable; handshakes deny until paired"
        );
    }

    #[test]
    fn deny_all_state_is_installed_only_for_invalid_auth_sections() {
        // A wss listener with no credential path is rejected by config
        // validation, so no supported surface can save it; if one is already
        // on disk the daemon still boots with an ordinary policy and the
        // listener denies every remote handshake. It is NOT a deny-all state.
        let mut unusable_listener = base_config();
        unusable_listener.wss.enabled = true;
        unusable_listener.gateway.require_pairing = false;
        assert!(
            unusable_listener.validate().is_err(),
            "config validation must reject a credential-path-less wss listener"
        );
        let auth = RpcInboundAuth::from_config(
            &unusable_listener,
            Arc::new(PairingGuard::new(false, &[], PairingCodePolicy::default())),
        )
        .expect("the daemon still boots so an operator can repair it");
        assert!(
            auth.resolve(&shared_operator_identity()).is_ok(),
            "the local shared operator keeps the repair path"
        );

        // An authorization section that does not compile is the deny-all
        // case: the roster references a profile that is not configured.
        let mut dangling = base_config();
        dangling.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                permission_profiles: vec!["not-configured".into()],
            },
        );
        let auth = RpcInboundAuth::from_config(
            &dangling,
            Arc::new(PairingGuard::new(false, &[], PairingCodePolicy::default())),
        )
        .expect("an invalid policy installs deny-all rather than refusing to load");
        assert!(
            auth.resolve(&shared_operator_identity()).is_err(),
            "a deny-all state refuses even the shared operator's ordinary grants"
        );
    }

    #[tokio::test]
    async fn deny_all_refuses_the_daemon_uid_so_only_the_on_disk_repair_remains() {
        // The Recovery section of docs/book/src/security/authentication.md
        // splits the two lockout states on exactly this behaviour: a policy
        // that compiles keeps the daemon's own uid on the trusted local path,
        // and a deny-all accepted state does not, so nothing reachable over
        // RPC repairs it.
        let mut dangling = base_config();
        dangling.security.trust_daemon_uid = true;
        dangling.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                permission_profiles: vec!["not-configured".into()],
            },
        );
        let daemon_uid = PeercredAuthProvider::current_process_uid();
        let auth = auth_for(&dangling, &["zc_tok"]);
        auth.authenticate(
            TransportKind::Local,
            Credential::Peercred { uid: daemon_uid },
            None,
            None,
        )
        .await
        .expect_err("a deny-all state refuses the daemon's own uid as well");

        // The same roster on a policy that compiles keeps that route open, so
        // the refusal above is the deny-all state and not the roster entry.
        let mut repaired = dangling;
        repaired
            .permission_profiles
            .insert("not-configured".into(), PermissionProfileConfig::default());
        let auth = auth_for(&repaired, &["zc_tok"]);
        auth.authenticate(
            TransportKind::Local,
            Credential::Peercred { uid: daemon_uid },
            None,
            None,
        )
        .await
        .expect("a compiling policy keeps the daemon uid on the trusted local path");
    }

    fn oidc_config() -> Config {
        let mut config = config_with_roster(4242);
        config.oidc.insert(
            "corp".into(),
            OidcConfig {
                issuer: "https://sso.example.com".into(),
                audience: "zeroclaw".into(),
                claim_path: "groups".into(),
                profile_map: std::collections::HashMap::from([("ops".into(), "operator".into())]),
                ..OidcConfig::default()
            },
        );
        config
    }

    /// The binding `initialize` produces for a verified OIDC access token.
    fn oidc_binding(auth: &RpcInboundAuth) -> ConnectionAuth {
        let serde_json::Value::Object(claims) = serde_json::json!({ "groups": ["ops"] }) else {
            unreachable!()
        };
        let identity = AuthenticatedIdentity::new(
            zeroclaw_api::principal::IdentitySubject::Oidc {
                issuer: "https://sso.example.com".into(),
                subject: "alice".into(),
            },
            AuthMethod::Oidc,
        )
        .with_provider_alias("corp")
        .with_claims(claims);
        let resolved = auth.resolve(&identity).expect("the OIDC identity resolves");
        ConnectionAuth {
            identity,
            principal: resolved.principal,
            grants: resolved.grants,
            generation: resolved.generation,
            native_token_hash: None,
            local_evidence: LocalCredentialEvidence::Oidc,
        }
    }

    #[test]
    fn resolve_current_keeps_an_oidc_binding_until_the_generation_moves() {
        let config = oidc_config();
        let auth = auth_for(&config, &[]);
        let binding = oidc_binding(&auth);

        let resolved = auth
            .resolve_current(&binding)
            .expect("an OIDC binding at its own generation stays usable");
        assert!(resolved.grants.permits(Resource::Sessions, Verb::Read));
        assert!(
            auth.revalidate_and_resolve(&binding).is_err(),
            "revalidation is for a changed generation, where an OIDC bearer cannot be reverified"
        );

        auth.refresh_from_config(&config)
            .expect("republishing the same policy compiles");
        assert!(
            auth.resolve_current(&binding).is_err(),
            "after a publication the OIDC binding must initialize again"
        );
    }

    #[test]
    fn publish_accepted_moves_the_generation_only_when_authorization_inputs_change() {
        let config = oidc_config();
        let auth = auth_for(&config, &[]);
        let binding = oidc_binding(&auth);
        let generation = auth.generation();

        let mut unrelated = config.clone();
        unrelated.gateway.port = unrelated.gateway.port.wrapping_add(1);
        assert_eq!(
            auth.publish_accepted(&unrelated, 1)
                .expect("an unrelated save publishes"),
            generation,
            "a save that changes no authorization input keeps the generation"
        );
        assert_eq!(auth.accepted_revision(), 1);
        auth.resolve_current(&binding)
            .expect("an OIDC binding survives an unrelated save");

        let mut narrowed = unrelated;
        narrowed
            .permission_profiles
            .insert("reader".into(), PermissionProfileConfig::default());
        let moved = auth
            .publish_accepted(&narrowed, 2)
            .expect("a profile change publishes");
        assert!(moved > generation, "a profile change moves the generation");
        assert!(
            auth.resolve_current(&binding).is_err(),
            "after an authorization change the OIDC binding must initialize again"
        );
    }

    /// A roster uid changed by a save must take effect on both sides: the old
    /// uid stops authenticating and the new one starts. Asserting only that the
    /// generation moved would pass even if the accepted roster never changed.
    #[tokio::test]
    async fn publishing_a_uid_swap_moves_authorization_to_the_new_uid() {
        let config = config_with_roster(4242);
        let auth = auth_for(&config, &[]);
        auth.authenticate(
            TransportKind::Local,
            Credential::Peercred { uid: 4242 },
            None,
            None,
        )
        .await
        .expect("the rostered uid authenticates before the swap");

        let mut swapped = config;
        swapped.users.get_mut("alice").unwrap().uid = Some(4343);
        auth.publish_accepted(&swapped, 1)
            .expect("a roster uid swap publishes");

        let denied = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: 4242 },
                None,
                None,
            )
            .await
            .expect_err("the superseded uid must stop authenticating");
        assert_eq!(denied.code, AUTH_REQUIRED);
        auth.authenticate(
            TransportKind::Local,
            Credential::Peercred { uid: 4343 },
            None,
            None,
        )
        .await
        .expect("the newly rostered uid authenticates after the swap");
    }

    /// Two writers publishing concurrently: the one that persisted an older
    /// revision must not reinstall its superseded policy over the newer one.
    /// The accepted roster, not just the revision counter, has to hold.
    #[tokio::test]
    async fn a_stale_concurrent_publication_cannot_reinstall_superseded_policy() {
        let config = config_with_roster(4242);
        let auth = auth_for(&config, &[]);

        let mut newer = config.clone();
        newer.users.get_mut("alice").unwrap().uid = Some(4343);
        auth.publish_accepted(&newer, 2)
            .expect("the newer revision publishes");

        // The slow writer's candidate still carries the old uid at revision 1.
        auth.publish_accepted(&config, 1)
            .expect("a stale publication is a no-op, not an error");
        assert_eq!(
            auth.accepted_revision(),
            2,
            "a stale publication must not move the accepted revision back"
        );

        let denied = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: 4242 },
                None,
                None,
            )
            .await
            .expect_err("the superseded uid must not be reinstated by a stale publish");
        assert_eq!(denied.code, AUTH_REQUIRED);
        auth.authenticate(
            TransportKind::Local,
            Credential::Peercred { uid: 4343 },
            None,
            None,
        )
        .await
        .expect("the newer roster still governs after the stale publish");
    }

    /// Removing daemon-uid trust must reach connections already established on
    /// it, not only new ones: the established binding has to revalidate and
    /// lose its authorization at the next resolve.
    #[tokio::test]
    async fn removing_daemon_uid_trust_denies_an_established_connection() {
        let mut trusting = config_with_roster(4242);
        trusting.security.trust_daemon_uid = true;
        let auth = auth_for(&trusting, &[]);
        let daemon_uid = PeercredAuthProvider::current_process_uid();
        let established = auth
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: daemon_uid },
                None,
                None,
            )
            .await
            .expect("the daemon uid is trusted before the change");
        auth.resolve_current(&established)
            .expect("the established connection resolves while the trust stands");

        let mut untrusting = trusting;
        untrusting.security.trust_daemon_uid = false;
        auth.publish_accepted(&untrusting, 1)
            .expect("removing daemon uid trust publishes");

        assert!(
            auth.resolve_current(&established).is_err(),
            "an established daemon-uid connection must lose authorization once the trust is removed"
        );
    }

    /// A connection established through the no-roster compatibility path must
    /// be re-evaluated once a roster exists: compatibility is the migration
    /// state, and adding a roster ends it for connections already open.
    #[tokio::test]
    async fn adding_a_roster_denies_an_established_compatibility_connection() {
        let compat = base_config();
        let auth = auth_for(&compat, &[]);
        // With no roster the socket mode is the credential: a local connection
        // presenting none is admitted as the shared operator.
        let established = auth
            .authenticate(TransportKind::Local, Credential::None, None, None)
            .await
            .expect("the compatibility path admits a local connection with no roster");
        auth.resolve_current(&established)
            .expect("the compatibility connection resolves while no roster exists");

        auth.publish_accepted(&config_with_roster(4343), 1)
            .expect("adding a roster publishes");

        assert!(
            auth.resolve_current(&established).is_err(),
            "adding a roster must end the compatibility admission for an open connection"
        );
    }

    /// Tightening `required_acr` through a save must reach connections already
    /// established on the old requirement, not only fresh tokens: the OIDC
    /// binding has to initialize again rather than keep resolving. Rejection of
    /// a fresh token that lacks the acr is covered by the provider's own tests
    /// in `security::auth_provider::oidc`.
    #[test]
    fn tightening_required_acr_forces_an_established_oidc_binding_to_reinitialize() {
        let config = oidc_config();
        let auth = auth_for(&config, &[]);
        let binding = oidc_binding(&auth);
        auth.resolve_current(&binding)
            .expect("the OIDC binding resolves under the accepted requirement");

        let mut tightened = config;
        tightened
            .oidc
            .get_mut("corp")
            .expect("the fixture provider exists")
            .required_acr = vec!["urn:example:assurance:mfa".into()];
        let moved = auth
            .publish_accepted(&tightened, 1)
            .expect("an ACR tightening publishes");
        assert!(
            moved > binding.generation,
            "tightening the acr requirement must move the generation"
        );
        assert!(
            auth.resolve_current(&binding).is_err(),
            "an established OIDC binding must initialize again after the acr tightens"
        );
    }

    #[test]
    fn publish_accepted_moves_the_generation_for_every_authorization_input() {
        type Mutation = fn(&mut Config);
        let mutations: [(&str, Mutation); 6] = [
            ("an OIDC field", |config| {
                config.oidc.get_mut("corp").unwrap().audience = "zeroclaw-next".into();
            }),
            ("a nested OIDC claim mapping", |config| {
                let corp = config.oidc.get_mut("corp").unwrap();
                corp.profile_map =
                    std::collections::HashMap::from([("admins".into(), "operator".into())]);
            }),
            ("a roster uid", |config| {
                config.users.get_mut("alice").unwrap().uid = Some(4343);
            }),
            ("a new roster entry", |config| {
                config.users.insert(
                    "bob".into(),
                    UserConfig {
                        principal_id: None,
                        uid: Some(4344),
                        permission_profiles: vec!["operator".into()],
                    },
                );
            }),
            ("a permission profile grant", |config| {
                config
                    .permission_profiles
                    .get_mut("operator")
                    .unwrap()
                    .grants
                    .insert(Resource::Cost, vec![Verb::Read]);
            }),
            ("the daemon uid trust posture", |config| {
                config.security.trust_daemon_uid = !config.security.trust_daemon_uid;
            }),
        ];
        for (input, mutate) in mutations {
            let config = oidc_config();
            let auth = auth_for(&config, &[]);
            let generation = auth.generation();
            let mut changed = config;
            mutate(&mut changed);
            let moved = auth
                .publish_accepted(&changed, 1)
                .unwrap_or_else(|e| panic!("{input}: the change publishes: {e}"));
            assert!(moved > generation, "{input} must move the generation");
        }
    }

    #[test]
    fn publish_accepted_replaces_a_deny_all_state_with_the_repaired_policy() {
        let mut dangling = base_config();
        dangling.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4242),
                permission_profiles: vec!["not-configured".into()],
            },
        );
        let auth = auth_for(&dangling, &[]);
        assert!(auth.resolve(&shared_operator_identity()).is_err());

        let mut repaired = dangling;
        repaired
            .permission_profiles
            .insert("not-configured".into(), PermissionProfileConfig::default());
        auth.publish_accepted(&repaired, 1)
            .expect("the repaired policy publishes");
        assert!(
            auth.resolve(&shared_operator_identity()).is_ok(),
            "publishing a compiling policy lifts the deny-all state"
        );
    }

    #[test]
    fn publish_accepted_refuses_an_older_revision() {
        let config = config_with_roster(4242);
        let auth = RpcInboundAuth::from_config(
            &config,
            Arc::new(PairingGuard::new(false, &[], PairingCodePolicy::default())),
        )
        .expect("the fixture policy compiles");
        assert_eq!(auth.accepted_revision(), 0);

        let first = auth
            .publish_accepted(&config, 1)
            .expect("the first publication installs");
        assert_eq!(auth.accepted_revision(), 1);
        assert_eq!(auth.generation(), first);

        // A writer that persisted revision 1 and published late must not
        // reinstall its policy over revision 2.
        let mut newer = config.clone();
        newer
            .permission_profiles
            .insert("reader".into(), PermissionProfileConfig::default());
        let second = auth
            .publish_accepted(&newer, 2)
            .expect("the newer publication installs");
        assert_eq!(auth.accepted_revision(), 2);

        let stale = auth
            .publish_accepted(&config, 1)
            .expect("a stale publication is a no-op, not an error");
        assert_eq!(stale, second, "the generation must not move");
        assert_eq!(auth.accepted_revision(), 2, "the revision must not move");

        assert!(
            auth.accepted_at_least(2).is_ok(),
            "the accepted state has reached revision 2"
        );
        assert!(
            auth.accepted_at_least(3).is_err(),
            "a revision that has not been published yet fails closed"
        );
    }
}
