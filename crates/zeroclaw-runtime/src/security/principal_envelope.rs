//! The principal envelope: a server-established, revocable delegation for
//! work that runs after its admitting connection is gone.
//!
//! A cron agent job, a SOP run, a headless driver, a delegation target, or a
//! channel-originated turn executes on a scheduler tick or a driver task, not
//! on the connection that admitted it. Copying the submitter's grants onto
//! the job would freeze them; carrying nothing would let the job run under
//! the agent's full policy.
//!
//! The envelope is minted by trusted admission (`PrincipalEnvelope::stamp`
//! from a `ConnectionAuth`, or `PrincipalEnvelope::trusted_internal` for
//! daemon-originated work) and never from client input. It records:
//!
//! - a stable delegation id, so the delegation can be revoked without
//!   touching the principal;
//! - the submitter's non-secret identity, for re-resolution;
//! - a credential reference (pairing token hash, peer uid, OIDC deadlines),
//!   so the credential that submitted the work is checked for liveness, not
//!   only the continued existence of the user;
//! - the origin (an RPC peer, or a named internal task);
//! - the grants held at admission, as an immutable ceiling;
//! - a format version.
//!
//! At execution, `PrincipalEnvelope::resolve_for_execution` refuses if the
//! delegation is revoked, if the credential is dead, or if the identity no
//! longer resolves; otherwise it returns the intersection of the fresh grants
//! with the ceiling as an `ExecutionGrants`, a type nothing else can build.
//! A later narrowing always applies; a later widening never exceeds the
//! ceiling. Whether a narrowing refuses the whole run or narrows its tool
//! set is the site's choice, made with `ExecutionGrants::require`.
//!
//! The tool ceiling for cron agent jobs is not defined here. The cron
//! dispatch defines it as agent policy ∩ requested tools ∩ submitter, and
//! refuses scoped submitters until the runtime can carry their delegated
//! authority to the tick. This envelope is the carrier that lifts that
//! refusal; the algebra stays with the cron dispatch.
//!
//! Missing provenance is never a route to authority: a persisted row without
//! an envelope, or with one this build cannot read, does not execute under
//! the agent's policy; the site refuses or quarantines it pending explicit
//! adoption.

use serde::{Deserialize, Serialize};
use zeroclaw_api::grants::{ResolvedGrants, Resource, Verb, WILDCARD};
use zeroclaw_api::principal::{
    AgentAlias, AuthMethod, AuthenticatedIdentity, IdentitySubject, PrincipalId,
};

use crate::rpc::auth::{AuthDenied, ConnectionAuth, LocalCredentialEvidence, RpcInboundAuth};

/// The envelope format this build writes and the newest it reads.
pub const ENVELOPE_VERSION: u32 = 1;

/// Identifies one delegation, so it can be revoked independently of the
/// principal and of any other delegation the same principal holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DelegationId(String);

impl DelegationId {
    fn mint() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the work was admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvelopeOrigin {
    /// Admitted over an RPC connection by an authenticated principal.
    Rpc,
    /// Started by the daemon itself, never by a request: the heartbeat, a
    /// SOP driver resumed from durable state, and the like.
    Internal { task: String },
    /// Written by a newer build.
    #[serde(other)]
    Unknown,
}

/// A non-secret reference to the credential that submitted the work, checked
/// for liveness at every execution. Never a bearer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialRef {
    /// Native pairing: the token hash, checked against the pairing authority.
    NativeTokenHash { hash: String },
    /// Unix peer credential: the uid; liveness is the roster mapping, which
    /// re-resolution checks.
    Peercred { uid: u32 },
    /// OIDC: liveness is the token's own expiry and revalidation deadline,
    /// carried on the identity.
    Oidc,
    /// The no-roster local compatibility path or the daemon's own uid.
    SharedOperator,
    /// Daemon-internal work; no external credential exists.
    Internal,
    #[serde(other)]
    Unknown,
}

/// Delegations an operator has revoked. One per daemon; sites pass it to
/// `resolve_for_execution`.
#[derive(Default)]
pub struct DelegationRevocations {
    revoked: std::sync::Mutex<std::collections::HashSet<DelegationId>>,
}

impl DelegationRevocations {
    pub fn revoke(&self, id: &DelegationId) {
        self.revoked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone());
    }

    pub fn is_revoked(&self, id: &DelegationId) -> bool {
        self.revoked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }
}

/// The authority a deferred effect carries.
#[derive(Clone)]
pub struct PrincipalEnvelope {
    delegation: DelegationId,
    version: u32,
    origin: EnvelopeOrigin,
    identity: AuthenticatedIdentity,
    credential: CredentialRef,
    submitted_by: PrincipalId,
    stamped_generation: u64,
    ceiling: ResolvedGrants,
}

impl std::fmt::Debug for PrincipalEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalEnvelope")
            .field("delegation", &self.delegation)
            .field("origin", &self.origin)
            .field("submitted_by", &self.submitted_by)
            .field("stamped_generation", &self.stamped_generation)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// Grants resolved for one execution of deferred work: the intersection of
/// the submitter's fresh authority with the envelope's ceiling. No public
/// constructor, so an execution path cannot be fed grants built elsewhere.
#[derive(Clone, Debug)]
pub struct ExecutionGrants {
    grants: ResolvedGrants,
    delegation: DelegationId,
    generation: u64,
    narrowed_from_ceiling: bool,
}

impl ExecutionGrants {
    pub fn grants(&self) -> &ResolvedGrants {
        &self.grants
    }

    pub fn delegation(&self) -> &DelegationId {
        &self.delegation
    }

    /// The generation the submitter's authority was resolved under.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the fresh authority is narrower than the ceiling somewhere.
    /// A site that must refuse rather than narrow checks this; one that
    /// narrows its tool set uses `grants()` directly.
    pub fn narrowed_from_ceiling(&self) -> bool {
        self.narrowed_from_ceiling
    }

    /// Refuse unless the right the execution consumes is still held. The
    /// denial names the right, so it is distinguishable from an unrelated
    /// reduction elsewhere in the ceiling.
    pub fn require(&self, resource: Resource, verb: Verb) -> Result<(), AuthDenied> {
        if self.grants.permits(resource, verb) {
            Ok(())
        } else {
            Err(AuthDenied::forbidden(format!(
                "delegation {} no longer holds {resource}:{verb}",
                self.delegation.as_str()
            )))
        }
    }
}

impl PrincipalEnvelope {
    /// Mint from the admitting connection's binding. Call at admission with
    /// the grants the gate stamped, never later, and never from anything a
    /// client sent.
    pub fn stamp(conn: &ConnectionAuth) -> Self {
        let credential = match &conn.local_evidence {
            LocalCredentialEvidence::NativeTokenHash => CredentialRef::NativeTokenHash {
                hash: conn.native_token_hash.clone().unwrap_or_default(),
            },
            LocalCredentialEvidence::Peercred { uid } => CredentialRef::Peercred { uid: *uid },
            LocalCredentialEvidence::Oidc => CredentialRef::Oidc,
            LocalCredentialEvidence::LocalCompatibility => CredentialRef::SharedOperator,
        };
        Self {
            delegation: DelegationId::mint(),
            version: ENVELOPE_VERSION,
            origin: EnvelopeOrigin::Rpc,
            identity: conn.identity.clone(),
            credential,
            submitted_by: conn.principal.id.clone(),
            stamped_generation: conn.generation,
            ceiling: conn.grants.clone(),
        }
    }

    /// Mint for work the daemon starts on its own authority. Only the
    /// daemon's own starters may call it, each naming its task; the authority
    /// ratchet forbids the identifier outside those starters.
    pub fn trusted_internal(task: impl Into<String>, ceiling: ResolvedGrants) -> Self {
        Self {
            delegation: DelegationId::mint(),
            version: ENVELOPE_VERSION,
            origin: EnvelopeOrigin::Internal { task: task.into() },
            identity: AuthenticatedIdentity::shared_operator(AuthMethod::SharedOperator),
            credential: CredentialRef::Internal,
            submitted_by: PrincipalId::shared_operator(),
            stamped_generation: 0,
            ceiling,
        }
    }

    pub fn delegation(&self) -> &DelegationId {
        &self.delegation
    }

    pub fn origin(&self) -> &EnvelopeOrigin {
        &self.origin
    }

    pub fn submitted_by(&self) -> &PrincipalId {
        &self.submitted_by
    }

    /// The grants held at admission. Execution never exceeds them.
    pub fn ceiling(&self) -> &ResolvedGrants {
        &self.ceiling
    }

    pub fn stamped_generation(&self) -> u64 {
        self.stamped_generation
    }

    /// At execution: check revocation and credential liveness, re-resolve
    /// the identity now, and intersect with the ceiling.
    pub fn resolve_for_execution(
        &self,
        inbound: &RpcInboundAuth,
        revocations: &DelegationRevocations,
    ) -> Result<ExecutionGrants, AuthDenied> {
        if revocations.is_revoked(&self.delegation) {
            return Err(AuthDenied::auth_required(format!(
                "delegation {} was revoked",
                self.delegation.as_str()
            )));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Some(expires_at) = self.identity.expires_at
            && expires_at <= now
        {
            return Err(AuthDenied::auth_required(
                crate::i18n::get_required_cli_string("rpc-auth-credential-expired"),
            ));
        }
        if let Some(revalidate_by) = self.identity.revalidate_by
            && revalidate_by <= now
        {
            return Err(AuthDenied::auth_required(
                crate::i18n::get_required_cli_string("rpc-auth-revalidation-due"),
            ));
        }
        match &self.credential {
            CredentialRef::NativeTokenHash { hash } => {
                if !inbound.pairing().token_hash_is_paired(hash) {
                    return Err(AuthDenied::auth_required(
                        crate::i18n::get_required_cli_string("rpc-auth-pairing-revoked"),
                    ));
                }
            }
            CredentialRef::Unknown => {
                return Err(AuthDenied::auth_required(format!(
                    "delegation {} carries a credential kind this build cannot verify",
                    self.delegation.as_str()
                )));
            }
            CredentialRef::Peercred { .. }
            | CredentialRef::Oidc
            | CredentialRef::SharedOperator
            | CredentialRef::Internal => {}
        }
        if self.origin == EnvelopeOrigin::Unknown {
            return Err(AuthDenied::auth_required(format!(
                "delegation {} has an origin this build cannot verify",
                self.delegation.as_str()
            )));
        }
        let resolved = inbound
            .resolve(&self.identity)
            .map_err(AuthDenied::from_deny_reason)?;
        let grants = intersect_grants(&resolved.grants, &self.ceiling);
        let narrowed_from_ceiling = grants != self.ceiling;
        Ok(ExecutionGrants {
            grants,
            delegation: self.delegation.clone(),
            generation: resolved.generation,
            narrowed_from_ceiling,
        })
    }

    /// The row form. Claim values travel with it because OIDC profile
    /// mapping reads them at re-resolution; the row is operator-private data
    /// and the values are the same ones the live connection holds.
    pub fn to_persisted(&self) -> PersistedEnvelope {
        PersistedEnvelope {
            version: self.version,
            delegation: self.delegation.clone(),
            origin: self.origin.clone(),
            credential: self.credential.clone(),
            subject: PersistedSubject::from_identity(&self.identity.subject),
            method: self.identity.method,
            provider_alias: self.identity.provider_alias.clone(),
            claims: self.identity.claims.clone(),
            mfa_verified: self.identity.mfa_verified,
            expires_at: self.identity.expires_at,
            revalidate_by: self.identity.revalidate_by,
            submitted_by: self.submitted_by.clone(),
            stamped_generation: self.stamped_generation,
            ceiling: self.ceiling.clone(),
        }
    }

    /// Rebuild from a persisted row. A newer format version or a subject
    /// kind this build does not know fails closed: the row cannot be
    /// re-resolved, so the work must not run.
    pub fn from_persisted(persisted: PersistedEnvelope) -> Result<Self, EnvelopeError> {
        if persisted.version > ENVELOPE_VERSION {
            return Err(EnvelopeError::NewerVersion(persisted.version));
        }
        let subject = persisted.subject.into_identity()?;
        let mut identity = AuthenticatedIdentity::new(subject, persisted.method)
            .with_claims(persisted.claims)
            .with_mfa_verified(persisted.mfa_verified);
        if let Some(alias) = persisted.provider_alias {
            identity = identity.with_provider_alias(alias);
        }
        if let Some(expires_at) = persisted.expires_at {
            identity = identity.with_expires_at(expires_at);
        }
        if let Some(revalidate_by) = persisted.revalidate_by {
            identity = identity.with_revalidate_by(revalidate_by);
        }
        Ok(Self {
            delegation: persisted.delegation,
            version: persisted.version,
            origin: persisted.origin,
            identity,
            credential: persisted.credential,
            submitted_by: persisted.submitted_by,
            stamped_generation: persisted.stamped_generation,
            ceiling: persisted.ceiling,
        })
    }
}

/// Why a persisted envelope could not be rebuilt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    UnknownSubject(String),
    NewerVersion(u32),
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSubject(kind) => write!(f, "unknown principal subject kind {kind:?}"),
            Self::NewerVersion(v) => write!(
                f,
                "envelope version {v} is newer than this build's {ENVELOPE_VERSION}"
            ),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// The row form. Field names are the wire contract; add fields with
/// `#[serde(default)]` only and bump `ENVELOPE_VERSION` when a reader must
/// refuse older builds' rows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedEnvelope {
    pub version: u32,
    pub delegation: DelegationId,
    pub origin: EnvelopeOrigin,
    pub credential: CredentialRef,
    pub subject: PersistedSubject,
    pub method: AuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_alias: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub claims: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub mfa_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revalidate_by: Option<u64>,
    pub submitted_by: PrincipalId,
    pub stamped_generation: u64,
    pub ceiling: ResolvedGrants,
}

/// A serializable mirror of `IdentitySubject`, which is `non_exhaustive`
/// and not serde. Refuses kinds it does not know.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersistedSubject {
    SharedOperator,
    Oidc {
        issuer: String,
        subject: String,
    },
    Service {
        issuer: String,
        client_id: String,
    },
    Roster {
        principal_id: String,
    },
    #[serde(other)]
    Unknown,
}

impl PersistedSubject {
    fn from_identity(subject: &IdentitySubject) -> Self {
        match subject {
            IdentitySubject::SharedOperator => Self::SharedOperator,
            IdentitySubject::Oidc { issuer, subject } => Self::Oidc {
                issuer: issuer.clone(),
                subject: subject.clone(),
            },
            IdentitySubject::Service { issuer, client_id } => Self::Service {
                issuer: issuer.clone(),
                client_id: client_id.clone(),
            },
            IdentitySubject::Roster { principal_id } => Self::Roster {
                principal_id: principal_id.clone(),
            },
            _ => Self::Unknown,
        }
    }

    fn into_identity(self) -> Result<IdentitySubject, EnvelopeError> {
        Ok(match self {
            Self::SharedOperator => IdentitySubject::SharedOperator,
            Self::Oidc { issuer, subject } => IdentitySubject::Oidc { issuer, subject },
            Self::Service { issuer, client_id } => IdentitySubject::Service { issuer, client_id },
            Self::Roster { principal_id } => IdentitySubject::Roster { principal_id },
            Self::Unknown => return Err(EnvelopeError::UnknownSubject("unknown".into())),
        })
    }
}

/// The intersection of two grant sets: what both allow.
///
/// `admin` on one side means "everything on this side", so the result is
/// the other side's explicit sets; `admin` on both stays `admin`. Selector
/// lists follow the repository's selector semantics: a `WILDCARD` on one
/// side yields the other side's list, and otherwise only names present on
/// both sides survive. Resource verbs intersect per resource.
pub fn intersect_grants(fresh: &ResolvedGrants, ceiling: &ResolvedGrants) -> ResolvedGrants {
    if fresh.admin && ceiling.admin {
        return ResolvedGrants::all();
    }
    let mut out = ResolvedGrants::none();
    out.admin = false;

    let fresh_agents: Vec<String> = fresh
        .allowed_agents
        .iter()
        .map(|a| a.as_str().to_owned())
        .collect();
    let ceiling_agents: Vec<String> = ceiling
        .allowed_agents
        .iter()
        .map(|a| a.as_str().to_owned())
        .collect();
    out.allowed_agents =
        intersect_selectors(fresh.admin, &fresh_agents, ceiling.admin, &ceiling_agents)
            .into_iter()
            .map(AgentAlias)
            .collect();
    out.allowed_tools = intersect_selectors(
        fresh.admin,
        &fresh.allowed_tools,
        ceiling.admin,
        &ceiling.allowed_tools,
    );
    // Config paths are prefix selectors; keep a path only if BOTH sides
    // grant it verbatim, or one side is unrestricted.
    out.config_write_paths = intersect_selectors(
        fresh.admin,
        &fresh.config_write_paths,
        ceiling.admin,
        &ceiling.config_write_paths,
    );

    out.resources = match (fresh.admin, ceiling.admin) {
        (true, false) => ceiling.resources.clone(),
        (false, true) => fresh.resources.clone(),
        _ => fresh
            .resources
            .iter()
            .filter_map(|(resource, verbs)| {
                let common: std::collections::BTreeSet<_> = ceiling
                    .resources
                    .get(resource)
                    .map(|c| verbs.intersection(c).copied().collect())
                    .unwrap_or_default();
                (!common.is_empty()).then_some((*resource, common))
            })
            .collect(),
    };
    out
}

fn intersect_selectors(
    fresh_admin: bool,
    fresh: &[String],
    ceiling_admin: bool,
    ceiling: &[String],
) -> Vec<String> {
    let fresh_all = fresh_admin || fresh.iter().any(|s| s == WILDCARD);
    let ceiling_all = ceiling_admin || ceiling.iter().any(|s| s == WILDCARD);
    match (fresh_all, ceiling_all) {
        (true, true) => vec![WILDCARD.to_owned()],
        (true, false) => ceiling.to_vec(),
        (false, true) => fresh.to_vec(),
        (false, false) => fresh
            .iter()
            .filter(|s| ceiling.contains(s))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeroclaw_config::pairing::{PairingCodePolicy, PairingGuard};
    use zeroclaw_config::schema::{Config, PermissionProfileConfig, UserConfig};

    use crate::rpc::transport::TransportKind;
    use crate::security::auth_provider::Credential;

    const ALICE_UID: u32 = 4343;

    fn config_with_alice(agents: &[&str], tools: &[&str]) -> Config {
        let mut config = Config::default();
        for alias in ["alpha", "beta"] {
            config.agents.insert(
                alias.to_string(),
                zeroclaw_config::schema::AliasedAgentConfig::default(),
            );
        }
        config.permission_profiles.insert(
            "submitter".into(),
            PermissionProfileConfig {
                grants: std::collections::HashMap::from([(
                    Resource::Cron,
                    vec![Verb::Create, Verb::Read],
                )]),
                allowed_agents: agents.iter().map(|a| (*a).to_string()).collect(),
                allowed_tools: tools.iter().map(|t| (*t).to_string()).collect(),
                ..PermissionProfileConfig::default()
            },
        );
        config.users.insert(
            "alice".into(),
            UserConfig {
                principal_id: None,
                uid: Some(ALICE_UID),
                permission_profiles: vec!["submitter".into()],
            },
        );
        config
    }

    fn inbound_for(config: &Config) -> RpcInboundAuth {
        RpcInboundAuth::from_config(
            config,
            Arc::new(PairingGuard::new(true, &[], PairingCodePolicy::default())),
        )
        .expect("valid policy")
    }

    async fn alice_on(inbound: &RpcInboundAuth) -> ConnectionAuth {
        inbound
            .authenticate(
                TransportKind::Local,
                Credential::Peercred { uid: ALICE_UID },
                None,
                None,
            )
            .await
            .expect("alice is on the roster")
    }

    #[tokio::test]
    async fn a_tick_after_the_submitter_is_narrowed_sees_the_narrowing() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let conn = alice_on(&inbound).await;
        let envelope = PrincipalEnvelope::stamp(&conn);
        let revocations = DelegationRevocations::default();
        assert!(envelope.ceiling().may_use_tool("file_read"));
        assert_eq!(envelope.origin(), &EnvelopeOrigin::Rpc);

        inbound
            .refresh_from_config(&config_with_alice(&["alpha"], &[]))
            .expect("narrowed policy compiles");

        let at_tick = envelope
            .resolve_for_execution(&inbound, &revocations)
            .expect("identity still resolves");
        assert!(!at_tick.grants().may_use_tool("file_read"));
        assert!(at_tick.narrowed_from_ceiling());
        assert!(at_tick.require(Resource::Cron, Verb::Read).is_ok());
        assert!(at_tick.require(Resource::Sessions, Verb::Read).is_err());
    }

    #[tokio::test]
    async fn a_widening_after_submission_never_exceeds_the_ceiling() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let conn = alice_on(&inbound).await;
        let envelope = PrincipalEnvelope::stamp(&conn);
        let revocations = DelegationRevocations::default();

        inbound
            .refresh_from_config(&config_with_alice(
                &["alpha", "beta"],
                &["file_read", "file_write"],
            ))
            .expect("widened policy compiles");

        let at_tick = envelope
            .resolve_for_execution(&inbound, &revocations)
            .expect("resolves");
        assert!(!at_tick.grants().may_use_tool("file_write"));
        assert!(!at_tick.grants().may_use_agent("beta"));
        assert!(at_tick.grants().may_use_tool("file_read"));
        assert!(!at_tick.narrowed_from_ceiling());
    }

    #[tokio::test]
    async fn a_revoked_delegation_cannot_execute_while_the_submitter_still_exists() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let conn = alice_on(&inbound).await;
        let envelope = PrincipalEnvelope::stamp(&conn);
        let revocations = DelegationRevocations::default();
        assert!(
            envelope
                .resolve_for_execution(&inbound, &revocations)
                .is_ok()
        );
        revocations.revoke(envelope.delegation());
        let denied = envelope
            .resolve_for_execution(&inbound, &revocations)
            .unwrap_err();
        assert!(denied.message.contains("revoked"), "{denied:?}");
    }

    #[tokio::test]
    async fn a_submitter_removed_from_the_roster_cannot_execute() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let conn = alice_on(&inbound).await;
        let envelope = PrincipalEnvelope::stamp(&conn);
        let mut without_alice = config_with_alice(&["alpha"], &["file_read"]);
        without_alice.users.clear();
        inbound
            .refresh_from_config(&without_alice)
            .expect("policy without alice compiles");
        assert!(
            envelope
                .resolve_for_execution(&inbound, &DelegationRevocations::default())
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_envelope_round_trips_through_its_persisted_form() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let conn = alice_on(&inbound).await;
        let envelope = PrincipalEnvelope::stamp(&conn);
        let json = serde_json::to_string(&envelope.to_persisted()).expect("serializes");
        let back: PersistedEnvelope = serde_json::from_str(&json).expect("deserializes");
        let rebuilt = PrincipalEnvelope::from_persisted(back).expect("rebuilds");
        assert_eq!(rebuilt.delegation(), envelope.delegation());
        assert_eq!(rebuilt.submitted_by(), envelope.submitted_by());
        assert_eq!(rebuilt.stamped_generation(), envelope.stamped_generation());
        let at_tick = rebuilt
            .resolve_for_execution(&inbound, &DelegationRevocations::default())
            .expect("resolves");
        assert!(at_tick.grants().may_use_tool("file_read"));
        assert_eq!(at_tick.delegation(), envelope.delegation());
    }

    #[test]
    fn an_unknown_subject_kind_or_newer_version_fails_closed() {
        let base = serde_json::json!({
            "version": 1,
            "delegation": "d-1",
            "origin": {"kind": "rpc"},
            "credential": {"kind": "oidc"},
            "subject": {"kind": "hardware_token", "serial": "x"},
            "method": "oidc",
            "submitted_by": "user:someone",
            "stamped_generation": 3,
            "ceiling": ResolvedGrants::none(),
        });
        let persisted: PersistedEnvelope = serde_json::from_value(base.clone()).expect("parses");
        assert_eq!(
            PrincipalEnvelope::from_persisted(persisted).unwrap_err(),
            EnvelopeError::UnknownSubject("unknown".into())
        );
        let mut newer = base;
        newer["version"] = serde_json::json!(99);
        newer["subject"] = serde_json::json!({"kind": "shared_operator"});
        let persisted: PersistedEnvelope = serde_json::from_value(newer).expect("parses");
        assert_eq!(
            PrincipalEnvelope::from_persisted(persisted).unwrap_err(),
            EnvelopeError::NewerVersion(99)
        );
    }

    #[tokio::test]
    async fn an_unknown_origin_or_credential_kind_is_refused_at_execution() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let conn = alice_on(&inbound).await;
        let mut persisted = PrincipalEnvelope::stamp(&conn).to_persisted();
        persisted.origin = EnvelopeOrigin::Unknown;
        let envelope = PrincipalEnvelope::from_persisted(persisted).expect("rebuilds");
        assert!(
            envelope
                .resolve_for_execution(&inbound, &DelegationRevocations::default())
                .is_err()
        );
        let mut persisted = PrincipalEnvelope::stamp(&conn).to_persisted();
        persisted.credential = CredentialRef::Unknown;
        let envelope = PrincipalEnvelope::from_persisted(persisted).expect("rebuilds");
        assert!(
            envelope
                .resolve_for_execution(&inbound, &DelegationRevocations::default())
                .is_err()
        );
    }

    #[tokio::test]
    async fn internal_work_carries_an_explicit_origin() {
        let inbound = inbound_for(&config_with_alice(&["alpha"], &["file_read"]));
        let mut ceiling = ResolvedGrants::none();
        ceiling
            .resources
            .insert(Resource::Cron, [Verb::Read].into_iter().collect());
        let envelope = PrincipalEnvelope::trusted_internal("heartbeat", ceiling);
        assert_eq!(
            envelope.origin(),
            &EnvelopeOrigin::Internal {
                task: "heartbeat".into()
            }
        );
        let at_tick = envelope
            .resolve_for_execution(&inbound, &DelegationRevocations::default())
            .expect("the shared operator resolves");
        // The ceiling caps the shared operator's full authority.
        assert!(!at_tick.grants().admin);
        assert!(at_tick.grants().permits(Resource::Cron, Verb::Read));
        assert!(!at_tick.grants().permits(Resource::Sessions, Verb::Read));
    }

    #[test]
    fn intersection_keeps_only_what_both_allow() {
        let mut fresh = ResolvedGrants::none();
        fresh.allowed_agents = vec![AgentAlias("alpha".into()), AgentAlias("beta".into())];
        fresh.allowed_tools = vec![WILDCARD.into()];
        fresh.resources.insert(
            Resource::Sessions,
            [Verb::Read, Verb::Update].into_iter().collect(),
        );
        let mut ceiling = ResolvedGrants::none();
        ceiling.allowed_agents = vec![AgentAlias("beta".into())];
        ceiling.allowed_tools = vec!["file_read".into()];
        ceiling
            .resources
            .insert(Resource::Sessions, [Verb::Read].into_iter().collect());
        ceiling
            .resources
            .insert(Resource::Cron, [Verb::Read].into_iter().collect());

        let out = intersect_grants(&fresh, &ceiling);
        assert!(!out.admin);
        assert!(out.may_use_agent("beta") && !out.may_use_agent("alpha"));
        assert!(out.may_use_tool("file_read") && !out.may_use_tool("shell"));
        assert!(out.permits(Resource::Sessions, Verb::Read));
        assert!(!out.permits(Resource::Sessions, Verb::Update));
        assert!(!out.permits(Resource::Cron, Verb::Read));
    }

    #[test]
    fn admin_on_one_side_yields_the_other_side() {
        let mut fresh = ResolvedGrants::none();
        fresh.allowed_tools = vec!["file_read".into()];
        let out = intersect_grants(&fresh, &ResolvedGrants::all());
        assert!(!out.admin);
        assert!(out.may_use_tool("file_read") && !out.may_use_tool("shell"));
        let both = intersect_grants(&ResolvedGrants::all(), &ResolvedGrants::all());
        assert!(both.admin);
    }
}
