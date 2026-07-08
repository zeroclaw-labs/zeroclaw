//! The authenticated subject — the single `Principal` every gateway/RPC/ACP
//! connection carries once an [`crate`] auth provider has verified a credential.
//!
//! This is the shared identity contract for RFC #7141 ("Authentication Provider
//! support"): each accepted auth provider (`oidc`, `ssh-key`, `peercred`, `native`)
//! verifies its own credential kind and emits ONE uniform [`Principal`] carrying
//! identity plus its asserted grant *claims*. Everything downstream — dispatch
//! authorization, audit, per-principal session/memory isolation — reads this type
//! and is therefore provider-agnostic.
//!
//! It lives in `zeroclaw-api` (the leaf crate) so it is importable by
//! `zeroclaw-runtime` (the auth engine + control plane), `zeroclaw-gateway` /
//! `zeroclaw-channels` (auth resolution), and any peer/A2A surface, with no
//! dependency cycle. The verification *engine* (the provider trait + registry +
//! IdP-claims→`Principal` mapping) lives in `zeroclaw-runtime/src/security/`; only
//! the data contract lives here.
//!
//! **This is a foundational seam, deliberately extensible — not frozen.**
//! [`AuthMethod`] and [`Principal`] are `#[non_exhaustive]`: a new provider adds an
//! `AuthMethod` arm + a provider impl, and the resolved-grant fields the RFC
//! requires (allowed-agents, config-write, admin/all — see [`Principal`]) are added
//! as additive `Principal` fields by the IamPolicy-wiring epic. Consumers match
//! `AuthMethod` with a `_` arm and construct `Principal` via [`Principal::new`];
//! both changes are non-breaking. Scoping the seam to the *accepted* #7141 set keeps
//! this PR from silently widening the security design ahead of the RFC decisions.
//!
//! Single source of truth: the legacy `NevisIdentity` (in
//! `zeroclaw-runtime/src/security/nevis.rs`) carries an overlapping identity+grants
//! shape today. Per RFC #7141 the `oidc` provider absorbs and **removes** it
//! (`NevisConfig` collapses into `oidc`), so this is the one forward identity type —
//! not a competing source of truth. The two coexist only until that provider lands.

use serde::{Deserialize, Serialize};

/// Stable, opaque subject id. The audit `Actor`, the approval-routing key, the
/// provenance origin, and (A2A) the peer join key. For an OIDC user this equals
/// the IdP `sub`; for the shared-bearer / trusted-local path it is the sentinel
/// [`PrincipalId::SHARED_OPERATOR`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalId(pub String);

impl PrincipalId {
    /// Sentinel id for the single-operator / trusted-local path (no distinct IdP
    /// principal). Lets callers treat "trusted, but anonymous operator" as a real
    /// `Principal` instead of branching on `Option`.
    pub const SHARED_OPERATOR: &'static str = "shared-operator";

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

/// An agent alias a principal may bind at session start. Newtype so it never gets
/// confused with an arbitrary `String` in grant checks.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentAlias(pub String);

impl AgentAlias {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a principal proved identity. The provider that authenticated it sets this.
///
/// Scoped to the **accepted RFC #7141 provider set** (OIDC, ssh-key, peercred,
/// native) plus the unbound/trusted sentinels. The enum is `#[non_exhaustive]`:
/// arms for not-yet-accepted providers are added additively by their own scoped
/// change (e.g. a local `Password` provider once its #7141 scope is accepted; a
/// `Peer` arm by the A2A effort), so landing them is never a silent widening of
/// this foundational security seam. Consumers match exhaustively with a `_` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthMethod {
    /// No authentication performed (default; an unbound connection).
    #[default]
    None,
    /// Explicitly-trusted connection with no distinct IdP principal — today's
    /// shared pairing bearer / trusted-local stdio. Carries the
    /// [`PrincipalId::SHARED_OPERATOR`] sentinel.
    SharedOperator,
    /// External OpenID Connect IdP (RFC #7141 headline provider).
    Oidc,
    /// Challenge-response against a registered SSH public key.
    SshKey,
    /// Local Unix-socket / named-pipe peer credential (`SO_PEERCRED`).
    Peercred,
    /// The existing `PairingGuard` bearer token (continuity / operator bootstrap).
    Native,
}

/// The single authenticated subject, produced by an auth provider and consumed by
/// every dispatch/authz/audit/isolation surface.
///
/// **Extensible, not frozen.** This struct is `#[non_exhaustive]` so later epics
/// can ADD fields without a breaking change. Construct it via [`Principal::new`]
/// plus the builder setters (or [`Principal::shared_operator`]), never a struct
/// literal from another crate.
///
/// Field semantics:
/// - [`Principal::id`] is the canonical join/attribution key (NOT `user_id`).
/// - `roles` / `scopes` are the **claims the identity source asserted** — they are
///   *inputs* to grant resolution, NOT ZeroClaw's resolved grants. The RFC #7141
///   resolved-grant shape (allowed agents, config-write paths, admin/all), computed
///   by `IamPolicy` from these claims, is added as additive fields in the
///   IamPolicy-wiring epic (#7142); `#[non_exhaustive]` keeps that non-breaking.
/// - `allowed_aliases` is today's coarse per-agent grant (the only resolved grant
///   carried so far).
/// - Constructed via a provider's `verify`, or [`Principal::shared_operator`] for
///   the trusted-local path. Never half-built.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Principal {
    /// Stable opaque id — the audit/approval/provenance origin and A2A join key.
    pub id: PrincipalId,
    /// Human/account identifier from the identity source (e.g. OIDC `sub`).
    /// Equals `id.0` for a real user; sentinel for [`AuthMethod::SharedOperator`].
    pub user_id: String,
    /// Coarse roles the identity source asserted (drives `IamPolicy` mapping).
    #[serde(default)]
    pub roles: Vec<String>,
    /// Fine-grained scopes/capabilities granted this session.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// How this principal authenticated.
    #[serde(default)]
    pub auth_method: AuthMethod,
    /// Whether a second factor was completed (drives any step-up policy).
    #[serde(default)]
    pub mfa_verified: bool,
    /// Session expiry, UNIX seconds; `0` = no expiry.
    #[serde(default)]
    pub expires_at: u64,
    /// Agent aliases this principal MAY bind at `session/new`. Empty + no roles ⇒
    /// the [`AuthMethod::SharedOperator`] fallback ("any configured alias",
    /// today's behaviour).
    #[serde(default)]
    pub allowed_aliases: Vec<AgentAlias>,
}

impl Principal {
    /// The sentinel principal for the shared-bearer / trusted-local path. No
    /// roles/scopes ⇒ authorization falls back to today's behaviour when no policy
    /// is configured. Lets callers carry a `Principal` everywhere instead of an
    /// `Option`, while [`Principal::is_authenticated`] still distinguishes it from
    /// a real IdP principal.
    #[must_use]
    pub fn shared_operator() -> Self {
        Self {
            id: PrincipalId(PrincipalId::SHARED_OPERATOR.to_owned()),
            user_id: PrincipalId::SHARED_OPERATOR.to_owned(),
            roles: Vec::new(),
            scopes: Vec::new(),
            auth_method: AuthMethod::SharedOperator,
            mfa_verified: false,
            expires_at: 0,
            allowed_aliases: Vec::new(),
        }
    }

    /// Construct an authenticated principal with the given subject id and method.
    /// Grants default to empty; attach claims via the `with_*` builder setters.
    /// This is the construction path other crates (the providers) must use because
    /// the struct is `#[non_exhaustive]`.
    #[must_use]
    pub fn new(
        id: impl Into<PrincipalId>,
        user_id: impl Into<String>,
        auth_method: AuthMethod,
    ) -> Self {
        Self {
            id: id.into(),
            user_id: user_id.into(),
            roles: Vec::new(),
            scopes: Vec::new(),
            auth_method,
            mfa_verified: false,
            expires_at: 0,
            allowed_aliases: Vec::new(),
        }
    }

    /// Attach the role claims the identity source asserted.
    #[must_use]
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }

    /// Attach the scope claims granted this session.
    #[must_use]
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = scopes;
        self
    }

    /// Mark MFA as completed.
    #[must_use]
    pub fn with_mfa_verified(mut self, mfa_verified: bool) -> Self {
        self.mfa_verified = mfa_verified;
        self
    }

    /// Set the session expiry (UNIX seconds; `0` = none).
    #[must_use]
    pub fn with_expires_at(mut self, expires_at: u64) -> Self {
        self.expires_at = expires_at;
        self
    }

    /// Attach the agent aliases this principal may bind.
    #[must_use]
    pub fn with_allowed_aliases(mut self, allowed_aliases: Vec<AgentAlias>) -> Self {
        self.allowed_aliases = allowed_aliases;
        self
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

/// Why a credential was rejected. Fail-closed: any ambiguity ⇒ a `Denied` variant,
/// never a silent allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// No credential was presented.
    NoCredential,
    /// A credential was presented but failed verification.
    BadCredential,
    /// The credential/session has expired.
    TokenExpired,
    /// A second factor is required and was not satisfied.
    MfaRequired,
    /// The principal is not entitled to the requested agent alias.
    AliasNotEntitled,
    /// The provider/config is misconfigured (fail closed, do not allow).
    Misconfigured,
}

/// The single result every auth surface returns. Misroute/timeout/malformed ⇒
/// [`AuthOutcome::Denied`], NEVER a silent allow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthOutcome {
    /// A distinct identity source authenticated the caller.
    Authenticated(Principal),
    /// An explicitly-trusted connection with no distinct IdP principal — carries
    /// the [`Principal::shared_operator`] sentinel so callers never branch on
    /// `Option`.
    Trusted(Principal),
    /// The credential was rejected.
    Denied { reason: DenyReason },
}

impl AuthOutcome {
    /// The bound principal if the outcome allows the connection (authenticated or
    /// trusted), else `None`.
    #[must_use]
    pub fn principal(&self) -> Option<&Principal> {
        match self {
            Self::Authenticated(p) | Self::Trusted(p) => Some(p),
            Self::Denied { .. } => None,
        }
    }

    /// Whether the connection is allowed to proceed at all (still subject to
    /// per-method grant checks downstream).
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Authenticated(_) | Self::Trusted(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_operator_is_trusted_but_not_authenticated() {
        let p = Principal::shared_operator();
        assert_eq!(p.id.as_str(), PrincipalId::SHARED_OPERATOR);
        assert_eq!(p.auth_method, AuthMethod::SharedOperator);
        assert!(!p.is_authenticated());
    }

    #[test]
    fn a_real_principal_is_authenticated() {
        let p = Principal {
            id: PrincipalId::from("alice"),
            user_id: "alice".to_owned(),
            roles: vec!["operator".to_owned()],
            scopes: vec![],
            auth_method: AuthMethod::Oidc,
            mfa_verified: true,
            expires_at: 0,
            allowed_aliases: vec![AgentAlias("main".to_owned())],
        };
        assert!(p.is_authenticated());
    }

    #[test]
    fn auth_outcome_allow_and_principal_accessors() {
        let ok = AuthOutcome::Trusted(Principal::shared_operator());
        assert!(ok.is_allowed());
        assert!(ok.principal().is_some());

        let no = AuthOutcome::Denied {
            reason: DenyReason::NoCredential,
        };
        assert!(!no.is_allowed());
        assert!(no.principal().is_none());
    }

    #[test]
    fn principal_roundtrips_through_json() {
        let p = Principal::shared_operator();
        let s = serde_json::to_string(&p).expect("serialize");
        let back: Principal = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn auth_method_serializes_snake_case() {
        let j = serde_json::to_string(&AuthMethod::SshKey).expect("serialize");
        assert_eq!(j, "\"ssh_key\"");
    }
}
