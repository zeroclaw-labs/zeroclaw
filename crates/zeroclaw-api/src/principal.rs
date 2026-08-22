//! The RFC 7141 identity contract: what an auth provider outputs
//! ([`AuthenticatedIdentity`]) and what the shared resolver turns it into
//! (the canonical [`Principal`]).
//!
//! Three stages, three vocabularies (Rev 8):
//!
//! 1. A provider verifies ONE credential and emits an [`AuthenticatedIdentity`]
//!    — provenance, subject, actor kind, verified claims, expiry/revalidation
//!    metadata. Providers never emit grants.
//! 2. The shared resolver (in `zeroclaw-runtime`) maps that identity to a
//!    canonical [`PrincipalId`] and the currently-configured permission
//!    profiles. Claims are mapping *inputs*, never runtime grants.
//! 3. Dispatch/gateway/storage surfaces consume the resolved [`Principal`]
//!    plus its separately-resolved [`crate::grants::ResolvedGrants`]. Grants
//!    are NOT stored on the `Principal`: they are re-resolved from current
//!    policy at the authorization-generation boundary, so a profile or
//!    mapping change affects established connections without reconnect.

use serde::{Deserialize, Serialize};

/// Canonical, globally-unambiguous principal identifier. The durable ownership
/// key for sessions, private memory, approvals, and audit attribution.
///
/// Composition is injective per identity class (see the constructors): an OIDC
/// identity is keyed by validated issuer AND subject — never by subject alone
/// and never by a config alias, so renaming an `[oidc.<alias>]` entry cannot
/// re-key principals, and two issuers cannot collide on a shared `sub`.
/// String equality across classes never links accounts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalId(pub String);

/// Escape `%` and `:` so a component can never smuggle the `:` join
/// separator — this is what makes the composed ids injective.
fn encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '%' => out.push_str("%25"),
            ':' => out.push_str("%3A"),
            _ => out.push(c),
        }
    }
    out
}

impl PrincipalId {
    /// Sentinel id for the single-operator / trusted-local path (no distinct
    /// identity source). Lets callers treat "trusted, but anonymous operator"
    /// as a real `Principal` instead of branching on `Option`.
    pub const SHARED_OPERATOR: &'static str = "shared-operator";

    #[must_use]
    pub fn shared_operator() -> Self {
        Self(Self::SHARED_OPERATOR.to_owned())
    }

    /// Canonical id for an OIDC human identity: validated issuer + `sub`.
    #[must_use]
    pub fn for_oidc(issuer: &str, subject: &str) -> Self {
        Self(format!(
            "oidc:{}:{}",
            encode_component(issuer),
            encode_component(subject)
        ))
    }

    /// Canonical id for an OIDC service principal: validated issuer + the
    /// stable verified client identity (introspected `client_id` or an
    /// equivalent provider-defined client subject). Never a human-subject
    /// mapping.
    #[must_use]
    pub fn for_service(issuer: &str, client_id: &str) -> Self {
        Self(format!(
            "svc:{}:{}",
            encode_component(issuer),
            encode_component(client_id)
        ))
    }

    /// Canonical id for a local roster identity: the durable
    /// `[users.<name>]` principal id (NOT the display name — renaming a
    /// roster entry must preserve this id or migrate ownership atomically).
    #[must_use]
    pub fn for_roster(roster_principal_id: &str) -> Self {
        Self(format!("user:{}", encode_component(roster_principal_id)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PrincipalId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PrincipalId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

/// An agent alias a principal may bind at session start. Newtype so it never
/// gets confused with an arbitrary `String` in grant checks.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentAlias(pub String);

impl AgentAlias {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What kind of actor an identity represents. Human and service identities
/// stay distinct end-to-end: a service principal never inherits interactive
/// browser sessions or private user memory because claims have similar names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActorKind {
    /// An interactive human user (or the shared operator).
    #[default]
    Human,
    /// A headless service principal (`client_credentials`).
    Service,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthMethod {
    /// No authentication performed (default; an unbound connection).
    #[default]
    None,
    /// Explicitly-trusted connection with no distinct identity — the
    /// shared pairing bearer / trusted-local stdio path. Carries the
    /// [`PrincipalId::SHARED_OPERATOR`] sentinel.
    SharedOperator,
    /// External OpenID Connect IdP (the RFC's headline provider).
    Oidc,
    /// Challenge-response against a registered SSH public key
    /// (separately-tracked extension; reserved here so the wire label is
    /// stable).
    SshKey,
    /// Local Unix-socket / named-pipe peer credential (`SO_PEERCRED`).
    Peercred,
    /// The existing `PairingGuard` bearer token (continuity / operator
    /// bootstrap).
    Native,
}

impl AuthMethod {
    /// Wire/audit label for the method — the `<type>` half of the
    /// `<type>.<alias>` provider attribution composite.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SharedOperator => "shared_operator",
            Self::Oidc => "oidc",
            Self::SshKey => "ssh-key",
            Self::Peercred => "peercred",
            Self::Native => "native",
        }
    }
}

/// The verified subject of an [`AuthenticatedIdentity`]. Each variant carries
/// exactly the fields its canonical [`PrincipalId`] is composed from, so a
/// provider cannot construct an ambiguous or cross-class identity.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentitySubject {
    /// Explicitly-trusted shared operator (native pairing token, or the
    /// daemon's own uid on the local socket). Trusted, but NOT a distinct
    /// authenticated identity.
    SharedOperator,
    /// An OIDC human identity. `issuer` is the VALIDATED `iss`; `subject`
    /// the validated `sub`.
    Oidc { issuer: String, subject: String },
    /// An OIDC service principal. `client_id` is the stable verified client
    /// identity from the validated token/introspection response.
    Service { issuer: String, client_id: String },
    /// A local roster identity: the durable `[users.<name>]` principal id
    /// the credential mapped to through explicit configuration.
    Roster { principal_id: String },
}

impl IdentitySubject {
    /// The canonical principal id this subject resolves to. One composition
    /// rule, owned here — providers and the resolver cannot disagree.
    #[must_use]
    pub fn principal_id(&self) -> PrincipalId {
        match self {
            Self::SharedOperator => PrincipalId::shared_operator(),
            Self::Oidc { issuer, subject } => PrincipalId::for_oidc(issuer, subject),
            Self::Service { issuer, client_id } => PrincipalId::for_service(issuer, client_id),
            Self::Roster { principal_id } => PrincipalId::for_roster(principal_id),
        }
    }

    /// The actor kind this subject implies.
    #[must_use]
    pub fn actor_kind(&self) -> ActorKind {
        match self {
            Self::Service { .. } => ActorKind::Service,
            _ => ActorKind::Human,
        }
    }

    /// The trust domain recorded on the resolved principal: the validated
    /// issuer for OIDC identities, `local` for roster identities, `native`
    /// for the shared operator.
    #[must_use]
    pub fn trust_domain(&self) -> &str {
        match self {
            Self::SharedOperator => "native",
            Self::Oidc { issuer, .. } | Self::Service { issuer, .. } => issuer,
            Self::Roster { .. } => "local",
        }
    }

    /// A human-readable, non-authoritative display identifier (OIDC `sub`,
    /// roster principal id, service `client_id`).
    #[must_use]
    pub fn display_id(&self) -> &str {
        match self {
            Self::SharedOperator => PrincipalId::SHARED_OPERATOR,
            Self::Oidc { subject, .. } => subject,
            Self::Service { client_id, .. } => client_id,
            Self::Roster { principal_id } => principal_id,
        }
    }
}

/// What an auth provider outputs after verifying ONE credential: identity and
/// evidence, never grants. Handshake-scoped and server-side only — this type
/// is deliberately not serializable, and its `Debug` never prints claim
/// values (claims may carry personal data).
#[derive(Clone)]
#[non_exhaustive]
pub struct AuthenticatedIdentity {
    /// The verified subject.
    pub subject: IdentitySubject,
    /// How the credential was verified.
    pub method: AuthMethod,
    /// The configured provider alias that verified it (e.g. `corp` for
    /// `[oidc.corp]`). `None` for providers with no config alias.
    pub provider_alias: Option<String>,
    /// Verified claims, as mapping INPUTS for the shared resolver's explicit
    /// claim-to-profile mapping. Empty for local/native identities. Never
    /// retained on the resolved [`Principal`].
    pub claims: serde_json::Map<String, serde_json::Value>,
    /// Whether the identity source attested a completed second factor.
    pub mfa_verified: bool,
    /// Credential expiry (UNIX seconds). `None` = the provider imposes none.
    pub expires_at: Option<u64>,
    /// Provider-specific revalidation deadline (UNIX seconds): the moment
    /// after which the next privileged operation must revalidate or fail
    /// closed. `None` = no revalidation requirement beyond `expires_at`.
    pub revalidate_by: Option<u64>,
}

impl AuthenticatedIdentity {
    /// Construct with empty claims and no expiry. Providers attach evidence
    /// via the `with_*` builders (the struct is `#[non_exhaustive]`).
    #[must_use]
    pub fn new(subject: IdentitySubject, method: AuthMethod) -> Self {
        Self {
            subject,
            method,
            provider_alias: None,
            claims: serde_json::Map::new(),
            mfa_verified: false,
            expires_at: None,
            revalidate_by: None,
        }
    }

    /// The trusted shared operator (native pairing / same-uid local peer).
    #[must_use]
    pub fn shared_operator(method: AuthMethod) -> Self {
        Self::new(IdentitySubject::SharedOperator, method)
    }

    #[must_use]
    pub fn with_provider_alias(mut self, alias: impl Into<String>) -> Self {
        self.provider_alias = Some(alias.into());
        self
    }

    #[must_use]
    pub fn with_claims(mut self, claims: serde_json::Map<String, serde_json::Value>) -> Self {
        self.claims = claims;
        self
    }

    #[must_use]
    pub fn with_mfa_verified(mut self, mfa_verified: bool) -> Self {
        self.mfa_verified = mfa_verified;
        self
    }

    #[must_use]
    pub fn with_expires_at(mut self, expires_at: u64) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    #[must_use]
    pub fn with_revalidate_by(mut self, revalidate_by: u64) -> Self {
        self.revalidate_by = Some(revalidate_by);
        self
    }

    /// The audit label for the verifying provider: `<type>.<alias>` when a
    /// config alias exists, else the bare method label.
    #[must_use]
    pub fn provider_label(&self) -> String {
        match &self.provider_alias {
            Some(alias) => format!("{}.{alias}", self.method.as_str()),
            None => self.method.as_str().to_owned(),
        }
    }
}

impl std::fmt::Debug for AuthenticatedIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatedIdentity")
            .field("subject", &self.subject)
            .field("method", &self.method)
            .field("provider_alias", &self.provider_alias)
            // Claim VALUES may carry personal data; keys are enough for debug.
            .field(
                "claims",
                &self.claims.keys().cloned().collect::<Vec<String>>(),
            )
            .field("mfa_verified", &self.mfa_verified)
            .field("expires_at", &self.expires_at)
            .field("revalidate_by", &self.revalidate_by)
            .finish()
    }
}

/// The canonical resolved principal: the single non-secret record every
/// dispatch/audit/isolation surface consumes.
///
/// Deliberately carries NO grants and NO raw claims: effective grants are
/// resolved from current permission-profile configuration by the shared
/// resolver and re-resolved when the authorization-policy generation
/// changes, so this record can be held for a connection's lifetime without
/// becoming a stale authorization snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Principal {
    /// Canonical ownership/audit key.
    pub id: PrincipalId,
    /// Human-readable, non-authoritative display identifier (OIDC `sub`,
    /// roster principal id, service `client_id`). Never an ownership key.
    pub display_id: String,
    /// Human or service actor.
    #[serde(default)]
    pub actor: ActorKind,
    /// How this principal authenticated.
    #[serde(default)]
    pub auth_method: AuthMethod,
    /// The `<alias>` of the configured provider entry that authenticated
    /// this principal, when one exists (audit attribution).
    #[serde(default)]
    pub provider_alias: Option<String>,
    /// The trust domain of the identity source (validated issuer URL,
    /// `local`, or `native`).
    #[serde(default)]
    pub trust_domain: String,
    /// Whether a second factor was completed.
    #[serde(default)]
    pub mfa_verified: bool,
    /// Credential expiry (UNIX seconds), when the provider imposes one.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Provider revalidation deadline (UNIX seconds), when one applies.
    #[serde(default)]
    pub revalidate_by: Option<u64>,
}

impl Principal {
    /// The single construction path: every `Principal` derives from a
    /// provider-verified [`AuthenticatedIdentity`], so no caller can invent
    /// a principal with an inconsistent id/actor/trust-domain combination.
    #[must_use]
    pub fn from_identity(identity: &AuthenticatedIdentity) -> Self {
        Self {
            id: identity.subject.principal_id(),
            display_id: identity.subject.display_id().to_owned(),
            actor: identity.subject.actor_kind(),
            auth_method: identity.method,
            provider_alias: identity.provider_alias.clone(),
            trust_domain: identity.subject.trust_domain().to_owned(),
            mfa_verified: identity.mfa_verified,
            expires_at: identity.expires_at,
            revalidate_by: identity.revalidate_by,
        }
    }

    /// The trusted shared-operator sentinel (no distinct identity source).
    #[must_use]
    pub fn shared_operator() -> Self {
        Self::from_identity(&AuthenticatedIdentity::shared_operator(
            AuthMethod::SharedOperator,
        ))
    }

    /// The audit label for the verifying provider: `<type>.<alias>` when a
    /// config alias exists, else the bare method label.
    #[must_use]
    pub fn auth_provider_label(&self) -> String {
        match &self.provider_alias {
            Some(alias) => format!("{}.{alias}", self.auth_method.as_str()),
            None => self.auth_method.as_str().to_owned(),
        }
    }

    /// `true` once a *distinct* identity source authenticated this principal —
    /// i.e. not unbound ([`AuthMethod::None`]) and not the shared-operator
    /// sentinel. A2A distinct-principal routing keys on this.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        !matches!(
            self.auth_method,
            AuthMethod::None | AuthMethod::SharedOperator
        )
    }
}

/// Why a credential was rejected. Fail-closed: any ambiguity ⇒ a `Denied`
/// variant, never a silent allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DenyReason {
    /// No credential was presented.
    NoCredential,
    /// A credential was presented but failed verification.
    BadCredential,
    /// The credential/session has expired.
    TokenExpired,
    /// A second factor is required and was not satisfied.
    MfaRequired,
    /// The credential authenticated but its claims/roster entry map to no
    /// permission profile, so the principal would hold no grants at all.
    NotEntitled,
    /// The principal is not entitled to the requested agent alias.
    AliasNotEntitled,
    /// No provider matches the requested provider selection.
    UnknownProvider,
    /// The provider/config is misconfigured (fail closed, do not allow).
    Misconfigured,
}

/// The single result every credential verification returns. Misroute,
/// timeout, or malformed input ⇒ [`AuthOutcome::Denied`], NEVER a silent
/// allow. Once a provider was selected for a credential, its denial is
/// authoritative: there is no fallback to another provider or to broader
/// authority.
#[derive(Clone, Debug)]
pub enum AuthOutcome {
    /// The selected provider verified the credential into an identity.
    /// Whether that identity is a *distinct* authenticated principal or the
    /// trusted shared operator is carried by [`IdentitySubject`].
    Verified(AuthenticatedIdentity),
    /// The credential was rejected.
    Denied { reason: DenyReason },
}

impl AuthOutcome {
    /// The verified identity if the outcome allows the connection, else
    /// `None`.
    #[must_use]
    pub fn identity(&self) -> Option<&AuthenticatedIdentity> {
        match self {
            Self::Verified(identity) => Some(identity),
            Self::Denied { .. } => None,
        }
    }

    /// Whether the connection may proceed at all (still subject to grant
    /// resolution and per-method checks downstream).
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Verified(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oidc_identity() -> AuthenticatedIdentity {
        AuthenticatedIdentity::new(
            IdentitySubject::Oidc {
                issuer: "https://sso.example.com/realms/main".into(),
                subject: "alice".into(),
            },
            AuthMethod::Oidc,
        )
        .with_provider_alias("corp")
    }

    #[test]
    fn shared_operator_is_trusted_but_not_authenticated() {
        let p = Principal::shared_operator();
        assert_eq!(p.id.as_str(), PrincipalId::SHARED_OPERATOR);
        assert_eq!(p.auth_method, AuthMethod::SharedOperator);
        assert_eq!(p.actor, ActorKind::Human);
        assert_eq!(p.trust_domain, "native");
        assert!(!p.is_authenticated());
    }

    #[test]
    fn oidc_principal_is_keyed_by_issuer_and_subject() {
        let p = Principal::from_identity(&oidc_identity());
        assert_eq!(
            p.id.as_str(),
            "oidc:https%3A//sso.example.com/realms/main:alice"
        );
        assert_eq!(p.display_id, "alice");
        assert_eq!(p.trust_domain, "https://sso.example.com/realms/main");
        assert!(p.is_authenticated());
    }

    #[test]
    fn principal_id_composition_is_injective_across_colon_boundaries() {
        // The classic splice: ("a:b", "c") must not equal ("a", "b:c").
        let left = PrincipalId::for_oidc("a:b", "c");
        let right = PrincipalId::for_oidc("a", "b:c");
        assert_ne!(left, right);

        // Percent signs in components cannot fake an escape sequence.
        let literal = PrincipalId::for_oidc("a%3A", "c");
        let colon = PrincipalId::for_oidc("a:", "c");
        assert_ne!(literal, colon);
    }

    #[test]
    fn same_subject_under_different_issuers_are_distinct_principals() {
        let a = PrincipalId::for_oidc("https://idp-a.example.com", "alice");
        let b = PrincipalId::for_oidc("https://idp-b.example.com", "alice");
        assert_ne!(a, b);
    }

    #[test]
    fn provider_alias_does_not_affect_the_principal_id() {
        let with_alias = Principal::from_identity(&oidc_identity());
        let mut identity = oidc_identity();
        identity.provider_alias = Some("renamed".into());
        let renamed = Principal::from_identity(&identity);
        assert_eq!(
            with_alias.id, renamed.id,
            "alias is audit-only, not identity"
        );
    }

    #[test]
    fn service_and_human_identities_never_collide() {
        let human = PrincipalId::for_oidc("https://idp.example.com", "svc-1");
        let service = PrincipalId::for_service("https://idp.example.com", "svc-1");
        assert_ne!(human, service);

        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Service {
                issuer: "https://idp.example.com".into(),
                client_id: "svc-1".into(),
            },
            AuthMethod::Oidc,
        );
        let p = Principal::from_identity(&identity);
        assert_eq!(p.actor, ActorKind::Service);
        assert!(p.is_authenticated());
    }

    #[test]
    fn roster_ids_are_prefixed_and_escaped() {
        assert_eq!(PrincipalId::for_roster("bob").as_str(), "user:bob");
        assert_eq!(
            PrincipalId::for_roster("b:ob").as_str(),
            "user:b%3Aob",
            "a roster id cannot smuggle a separator"
        );
        let identity = AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "bob".into(),
            },
            AuthMethod::Peercred,
        );
        let p = Principal::from_identity(&identity);
        assert_eq!(p.trust_domain, "local");
        assert!(p.is_authenticated());
    }

    #[test]
    fn auth_outcome_allow_and_identity_accessors() {
        let ok = AuthOutcome::Verified(oidc_identity());
        assert!(ok.is_allowed());
        assert!(ok.identity().is_some());

        let no = AuthOutcome::Denied {
            reason: DenyReason::NoCredential,
        };
        assert!(!no.is_allowed());
        assert!(no.identity().is_none());
    }

    #[test]
    fn every_deny_reason_is_not_allowed() {
        for reason in [
            DenyReason::NoCredential,
            DenyReason::BadCredential,
            DenyReason::TokenExpired,
            DenyReason::MfaRequired,
            DenyReason::NotEntitled,
            DenyReason::AliasNotEntitled,
            DenyReason::UnknownProvider,
            DenyReason::Misconfigured,
        ] {
            let outcome = AuthOutcome::Denied { reason };
            assert!(!outcome.is_allowed());
            assert!(outcome.identity().is_none());
        }
    }

    #[test]
    fn principal_roundtrips_through_json() {
        let p =
            Principal::from_identity(&oidc_identity().with_mfa_verified(true).with_expires_at(42));
        let s = serde_json::to_string(&p).expect("serialize");
        let back: Principal = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn auth_method_serializes_snake_case() {
        let j = serde_json::to_string(&AuthMethod::SshKey).expect("serialize");
        assert_eq!(j, "\"ssh_key\"");
    }

    #[test]
    fn provider_label_prefers_configured_alias() {
        let labeled = Principal::from_identity(&oidc_identity());
        assert_eq!(labeled.auth_provider_label(), "oidc.corp");
        let bare = Principal::from_identity(&AuthenticatedIdentity::new(
            IdentitySubject::Roster {
                principal_id: "bob".into(),
            },
            AuthMethod::Peercred,
        ));
        assert_eq!(bare.auth_provider_label(), "peercred");
    }

    #[test]
    fn identity_debug_never_prints_claim_values() {
        let mut claims = serde_json::Map::new();
        claims.insert(
            "email".into(),
            serde_json::Value::String("alice@example.com".into()),
        );
        let identity = oidc_identity().with_claims(claims);
        let dbg = format!("{identity:?}");
        assert!(dbg.contains("email"), "claim keys are shown");
        assert!(
            !dbg.contains("alice@example.com"),
            "claim values must never appear in debug output"
        );
    }

    #[test]
    fn identity_evidence_flows_onto_the_principal() {
        let identity = oidc_identity()
            .with_mfa_verified(true)
            .with_expires_at(1000)
            .with_revalidate_by(500);
        let p = Principal::from_identity(&identity);
        assert!(p.mfa_verified);
        assert_eq!(p.expires_at, Some(1000));
        assert_eq!(p.revalidate_by, Some(500));
        assert_eq!(identity.provider_label(), "oidc.corp");
    }
}
