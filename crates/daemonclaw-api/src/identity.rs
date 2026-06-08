//! Agent identity layer — WardToken-managed Ed25519 identity for Oni.
//!
//! This module defines the [`IdentityProvider`] trait and its associated
//! types. The trait is the contract between the rest of the daemon (the
//! agent loop, the hub, the gateway) and any concrete identity backend.
//! The default implementation is `LocalIdentityProvider` in the
//! `daemonclaw-identity` crate; the optional `daemonclaw-wardtoken` crate
//! provides a remote-issuer variant.
//!
//! ## Design invariants
//!
//! 1. **Two-step verify.** The `verify` method on this trait is the
//!    *consuming* side: it (a) checks the Ed25519 signature locally against
//!    the registered public key, then (b) calls the issuer's `/verify`
//!    endpoint. The local check fails closed (bad signature = no); the
//!    issuer check reports liveness + scopes.
//!
//! 2. **Four-state `IssuerStatus`.** First boot is `Unqueried`, not
//!    `Authorized`. A never-attempted status is operationally different
//!    from a tried-but-denied status, and a tried-but-unreachable status.
//!    `Authorized` is reachable only via a real 200.
//!
//! 3. **No private-key leakage in Display/Debug.** Any type that may
//!    carry a signature or a private key has a hand-rolled `Debug` impl
//!    that omits those fields. Tests assert this on every type.
//!
//! 4. **`fingerprint` is the cross-system contract.** Oni's `fingerprint`
//!    string and WardToken's `fingerprint_der` must be byte-identical for
//!    the same public key. A known-vector test pins the format in both
//!    crates; an SPKI round-trip test pins the bytes that feed it.
//!
//! 5. **No fallbacks.** An unknown `provider` is an error, not a graceful
//!    degradation. Misconfiguration should be loud.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Public types ─────────────────────────────────────────────────

/// A view of the agent's own identity, suitable for `whoami` and status
/// surfaces.
///
/// `key_fingerprint` and `issuer_status.scopes` are the only fields that
/// may be absent; everything else is always known once the provider has
/// been initialized. The `Debug` impl intentionally omits no fields but
/// still respects the no-private-key invariant (no private key fields here).
#[derive(Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Oni's user-id UUID (the agent's row in WardToken). For the
    /// `LocalIdentityProvider` this is a stable locally-generated UUID.
    pub agent_user_id: String,
    /// Human-readable label for the agent (e.g. "Oni", "claw").
    pub label: String,
    /// `sha256:<base64url-no-pad>` of `SHA256(SPKI_DER)`. The byte format
    /// must match WardToken's `fingerprint_der` exactly — pinned by
    /// known-vector test.
    pub key_fingerprint: Option<String>,
    /// SPKI PEM (public key) — `-----BEGIN PUBLIC KEY----- ... -----END`.
    /// Stable, public, safe to log at INFO.
    pub spki_pem: Option<String>,
    /// `KeyRegistered` for the local floor; `CaSigned` is reserved for a
    /// later track that does not exist today.
    pub tier: TrustTier,
    /// Four-state enum — see the type docs.
    pub issuer_status: IssuerStatus,
}

impl std::fmt::Debug for AgentIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SPKI PEM and fingerprint are public; safe to show. Scopes are
        // scope names, not values; safe.
        f.debug_struct("AgentIdentity")
            .field("agent_user_id", &self.agent_user_id)
            .field("label", &self.label)
            .field("key_fingerprint", &self.key_fingerprint)
            .field("spki_pem", &self.spki_pem.as_ref().map(|s| redact_pem(s)))
            .field("tier", &self.tier)
            .field("issuer_status", &self.issuer_status)
            .finish()
    }
}

impl std::fmt::Display for AgentIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "agent={} label={} tier={} issuer={}",
            self.agent_user_id,
            self.label,
            self.tier,
            self.issuer_status
        )
    }
}

/// What tier of trust does the registered key carry?
///
/// The floor is `KeyRegistered` — Oni generated a key, persisted it
/// locally, and it hasn't been revoked. A future track may add a CA-signed
/// tier, but WardToken v1 does not expose one. The enum is binary so
/// the future addition is additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustTier {
    /// Local Ed25519 key generated and loaded. Floor.
    KeyRegistered,
    /// Reserved for a future CA-signed tier. Not expressible today.
    CaSigned,
}

impl std::fmt::Display for TrustTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyRegistered => write!(f, "key-registered"),
            Self::CaSigned => write!(f, "ca-signed"),
        }
    }
}

/// Operational state of the issuer relationship.
///
/// Four states, not three. The distinctions are load-bearing for
/// `daemonclaw status` and incident response:
///
/// - `Unqueried` — first boot, or no verify call yet attempted. The
///   operator's next step is to register the key in the issuer.
/// - `Authorized` — verify returned 200 with non-empty scopes. The
///   agent is live and authorized. Reachable only via a real 200.
/// - `Unauthorized` — verify returned 403 (no grant, revoked, or
///   fingerprint mismatch). Operator action required.
/// - `Unreachable` — network error, timeout, or 5xx. Self-heals on
///   next verify call. Don't page anyone.
#[derive(Clone, Serialize, Deserialize)]
pub enum IssuerStatus {
    /// Boot before any verify call attempted. Never trust this as
    /// "we're authorized"; the operator's key might not be registered
    /// yet, which is fine and expected.
    Unqueried,
    /// Verify returned 200 with non-empty scopes. The agent is live
    /// and authorized. Scopes are scope names (e.g. "profile"), not
    /// values.
    Authorized { scopes: Vec<String> },
    /// Verify returned 403. Either no grant, revoked, or fingerprint
    /// mismatch. Cross-system contract: a 403 from format-mismatched
    /// fingerprint and a 403 from "no grant" are indistinguishable by
    /// design (anti-enumeration).
    Unauthorized,
    /// Network error, timeout, or 5xx. Self-heals on next attempt.
    /// `last_error` is for human inspection; `last_attempt` is the
    /// unix timestamp at the time of the failure.
    Unreachable {
        last_error: String,
        last_attempt: i64,
    },
}

impl std::fmt::Debug for IssuerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unqueried => write!(f, "Unqueried"),
            Self::Authorized { scopes } => f
                .debug_struct("Authorized")
                .field("scopes", scopes)
                .finish(),
            Self::Unauthorized => write!(f, "Unauthorized"),
            Self::Unreachable { last_error, last_attempt } => f
                .debug_struct("Unreachable")
                .field("last_error", last_error)
                .field("last_attempt", last_attempt)
                .finish(),
        }
    }
}

impl std::fmt::Display for IssuerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unqueried => write!(f, "unqueried"),
            Self::Authorized { scopes } => write!(f, "authorized(scopes={})", scopes.join(",")),
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::Unreachable { last_error, last_attempt } => write!(
                f,
                "unreachable(at={}, err={})",
                last_attempt, last_error
            ),
        }
    }
}

/// A signed assertion — what Oni emits to prove it holds the key.
///
/// The `signature` field carries the raw Ed25519 signature over the
/// canonical bytes of the other fields. The serialization is a
/// length-prefixed concatenation; both ends of the wire agree on it.
///
/// Debug intentionally elides `signature` (raw bytes — not a secret, but
/// not useful to print at INFO).
#[derive(Clone, Serialize, Deserialize)]
pub struct IdentityAssertion {
    pub agent_user_id: String,
    pub grantor_user_id: String,
    pub fingerprint: String,
    /// Optional audience binding (e.g. "ward.deliveryboy.tech" or a
    /// hub session id). When `None`, the assertion is non-audience-bound.
    pub audience: Option<String>,
    /// Unix timestamp (seconds) when the assertion was created.
    pub issued_at: i64,
    /// Random nonce — single-use protection against replay. Generated
    /// by the signer, validated by the verifier (caller's responsibility).
    pub nonce: String,
    /// Ed25519 signature over the canonical serialization of the
    /// other fields (length-prefixed concatenation, in field order).
    pub signature: Vec<u8>,
}

impl std::fmt::Debug for IdentityAssertion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityAssertion")
            .field("agent_user_id", &self.agent_user_id)
            .field("grantor_user_id", &self.grantor_user_id)
            .field("fingerprint", &self.fingerprint)
            .field("audience", &self.audience)
            .field("issued_at", &self.issued_at)
            .field("nonce", &redact_nonce(&self.nonce))
            .field("signature", &format!("<{} bytes>", self.signature.len()))
            .finish()
    }
}

/// Result of a two-step verify (signature check + issuer liveness check).
///
/// This is what the *consuming* side (the hub, the gateway) sees. It
/// reports both flags independently so the consumer can decide what to
/// do. A `signature_ok = true` with `IssuerStatus::Unauthorized` means
/// the signature checked out, but the issuer says the grant is gone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Local Ed25519 check against the registered public key.
    pub signature_ok: bool,
    /// Issuer liveness check (`/verify` 200/403/timeout).
    pub issuer_status: IssuerStatus,
    /// Single failure reason if verification failed. `None` if both
    /// checks succeeded.
    pub failure_reason: Option<VerifyFailure>,
}

/// Why a verify call did not succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerifyFailure {
    /// Ed25519 signature did not validate against the registered pubkey.
    BadSignature,
    /// Issuer endpoint unreachable (network/timeout/5xx). Not a
    /// authorization failure — self-heals on next call.
    IssuerUnreachable,
    /// Issuer returned 403 (no grant, revoked, or fingerprint mismatch).
    /// Anti-enumeration: indistinguishable from format mismatch by design.
    IssuerDenied,
}

impl std::fmt::Display for VerifyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSignature => write!(f, "bad-signature"),
            Self::IssuerUnreachable => write!(f, "issuer-unreachable"),
            Self::IssuerDenied => write!(f, "issuer-denied"),
        }
    }
}

// ── Trait ────────────────────────────────────────────────────────

/// Provides the agent's identity — local key handling, assertion signing,
/// and the consuming-side two-step verify (signature + issuer liveness).
///
/// The trait is the integration surface. Every concrete impl is a
/// `Box<dyn IdentityProvider>`. There is no default method for `verify`
/// because the two checks are issuer-specific (who to call, what URL,
/// what error to map).
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// Backend name. Used for `daemonclaw status` and logs.
    fn name(&self) -> &str;

    /// The local view of the agent. Never panics; never returns
    /// `Authorized` unless a real 200 has been observed. `whoami` is
    /// cheap and may be called frequently.
    async fn whoami(&self) -> anyhow::Result<AgentIdentity>;

    /// Sign an assertion for the given audience. Audience-bound
    /// assertions are recommended for any non-trivial use. The
    /// `nonce` is generated by the implementation and embedded in the
    /// returned `IdentityAssertion`.
    async fn assertion(
        &self,
        audience: Option<&str>,
    ) -> anyhow::Result<IdentityAssertion>;

    /// Consuming-side two-step verify.
    ///
    /// (1) Check `assertion.signature` against the registered public
    /// key. (2) Call the issuer's `/verify` to confirm liveness and
    /// authorization. Both flags are reported independently; the
    /// failure reason is set when either check fails.
    async fn verify(
        &self,
        assertion: &IdentityAssertion,
    ) -> anyhow::Result<VerificationResult>;
}

// ── Helpers ──────────────────────────────────────────────────────

/// Redact a PEM body, keeping the BEGIN/END markers but collapsing
/// the base64 lines to `<...bytes...>`. Public-key material is not a
/// secret, but long base64 blobs in logs are noise.
fn redact_pem(pem: &str) -> String {
    let mut out = String::new();
    for line in pem.lines() {
        if line.starts_with("-----") {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str("<...bytes...>\n");
        }
    }
    out
}

/// First 4 + last 4 of the nonce, useful for correlating without
/// printing the full value. Nonces are public (they're in the
/// assertion), but again — log noise.
fn redact_nonce(n: &str) -> String {
    if n.len() <= 12 {
        return "<nonce>".into();
    }
    format!("{}…{}", &n[..4], &n[n.len() - 4..])
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuer_status_display_outputs_four_distinct_states() {
        // Four states must round-trip through Display with distinct output.
        // If someone collapses two, this test fails before any contract
        // bug does.
        let s = vec![
            IssuerStatus::Unqueried,
            IssuerStatus::Authorized {
                scopes: vec!["profile".into()],
            },
            IssuerStatus::Unauthorized,
            IssuerStatus::Unreachable {
                last_error: "connection refused".into(),
                last_attempt: 1_700_000_000,
            },
        ];
        let ds: Vec<String> = s.iter().map(|x| x.to_string()).collect();
        assert_eq!(ds[0], "unqueried");
        assert!(ds[1].starts_with("authorized("));
        assert_eq!(ds[2], "unauthorized");
        assert!(ds[3].starts_with("unreachable("));
        // All four are distinct.
        let unique: std::collections::HashSet<_> = ds.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn issuer_status_debug_redacts_acceptable() {
        // Debug output must not be the silent fallback (e.g. "{:?}"). Must
        // explicitly include the variant name.
        let s = IssuerStatus::Authorized {
            scopes: vec!["profile".into(), "offline_access".into()],
        };
        let d = format!("{s:?}");
        assert!(d.contains("Authorized"), "Debug must show variant name: {d}");
        assert!(d.contains("profile"));
    }

    #[test]
    fn verify_failure_display_distinct() {
        // Three failure modes must have distinct display strings so
        // operators reading logs can tell them apart at a glance.
        let s = vec![
            VerifyFailure::BadSignature,
            VerifyFailure::IssuerUnreachable,
            VerifyFailure::IssuerDenied,
        ];
        let ds: Vec<String> = s.iter().map(|x| x.to_string()).collect();
        let unique: std::collections::HashSet<_> = ds.iter().collect();
        assert_eq!(unique.len(), 3, "failures must be distinct: {ds:?}");
    }

    #[test]
    fn trust_tier_display_outputs_two_states() {
        assert_eq!(TrustTier::KeyRegistered.to_string(), "key-registered");
        assert_eq!(TrustTier::CaSigned.to_string(), "ca-signed");
    }

    #[test]
    fn agent_identity_debug_does_not_panic_on_all_variants() {
        // Quick smoke test: Debug/Display on AgentIdentity across all
        // issuer states. Doesn't assert specific output — just that
        // nothing panics (e.g. trying to format None fields).
        let variations = vec![
            AgentIdentity {
                agent_user_id: "u1".into(),
                label: "Oni".into(),
                key_fingerprint: Some("sha256:abc".into()),
                spki_pem: None,
                tier: TrustTier::KeyRegistered,
                issuer_status: IssuerStatus::Unqueried,
            },
            AgentIdentity {
                agent_user_id: "u1".into(),
                label: "Oni".into(),
                key_fingerprint: Some("sha256:abc".into()),
                spki_pem: Some(
                    "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA...\n-----END PUBLIC KEY-----\n"
                        .into(),
                ),
                tier: TrustTier::KeyRegistered,
                issuer_status: IssuerStatus::Authorized {
                    scopes: vec!["profile".into()],
                },
            },
            AgentIdentity {
                agent_user_id: "u1".into(),
                label: "Oni".into(),
                key_fingerprint: None,
                spki_pem: None,
                tier: TrustTier::KeyRegistered,
                issuer_status: IssuerStatus::Unreachable {
                    last_error: "timeout".into(),
                    last_attempt: 1,
                },
            },
        ];
        for v in &variations {
            let _ = format!("{v:?}");
            let _ = v.to_string();
        }
    }

    #[test]
    fn assertion_debug_elides_signature() {
        // Debug must not print 64 raw bytes — the field is shown as
        // a redacted count instead. Public assertion, but log noise.
        let a = IdentityAssertion {
            agent_user_id: "u".into(),
            grantor_user_id: "g".into(),
            fingerprint: "sha256:abc".into(),
            audience: Some("hub".into()),
            issued_at: 1_700_000_000,
            nonce: "0123456789abcdef0123".into(),
            signature: vec![0u8; 64],
        };
        let d = format!("{a:?}");
        assert!(
            !d.contains("0, 0, 0, 0"),
            "Debug must not print raw signature bytes: {d}"
        );
        assert!(d.contains("<64 bytes>"), "Debug must show byte count: {d}");
    }

    #[test]
    fn redact_pem_keeps_markers() {
        let pem = "-----BEGIN PUBLIC KEY-----\nABCDEF\nGHIJKL\n-----END PUBLIC KEY-----\n";
        let r = redact_pem(pem);
        assert!(r.contains("-----BEGIN PUBLIC KEY-----"));
        assert!(r.contains("-----END PUBLIC KEY-----"));
        assert!(r.contains("<...bytes...>"));
        assert!(!r.contains("ABCDEF"), "base64 must be redacted: {r}");
    }

    #[test]
    fn redact_nonce_short_and_long() {
        assert_eq!(redact_nonce("ab"), "<nonce>");
        assert_eq!(
            redact_nonce("0123456789abcdef0123"),
            "0123…0123"
        );
    }
}
