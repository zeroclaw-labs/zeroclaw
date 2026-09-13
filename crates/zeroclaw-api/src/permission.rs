//! Trusted approval confirmation contract — the frozen Phase 0 boundary of
//! RFC 7155.
//!
//! These types are the durable cross-surface contract for what an approval
//! IS: a [`TrustedConfirmation`] binds the approver's decision to the exact
//! approved action via an [`ActionFingerprint`], records who decided and
//! through which trusted route, stays valid for one [`TimeWindow`], and is
//! consumed at most once with deterministic terminal states
//! ([`ConsumeOutcome`]). The runtime approval gate, channel approval
//! backchannels, the gateway, and future consumers (stronger approval
//! authentication issue 3767, the desktop approval consumer issue 6909, the separately
//! ratified automated-approver phase) all reference this module so the
//! contract cannot drift per-surface.
//!
//! Two invariants are the point of the whole module:
//!
//! 1. **Provenance.** A confirmation can only be minted by the trusted
//!    approval path — the runtime gate, after a real operator or backchannel
//!    answer. Nothing in the model's tool-call JSON can produce one
//!    (RFC 7155 §5.1 removed the model-self-attestable `approved` field).
//! 2. **Exactness.** The fingerprint binds the complete action facts, not
//!    the display string. Any change to the action — arguments, working
//!    directory, env changes, redirections, principal — makes an existing
//!    approval `Stale` rather than `Consumed`.
//!
//! The rule engine that decides `Deny`/`Ask`/`Allow` before any of this
//! matters lives in `zeroclaw-config::tool_policy`; this module only
//! carries the confirmation that an `Ask` was answered and what it
//! authorized.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::principal::Principal;

/// Domain-separation tag for [`ActionFingerprint`] fact schema v1.
///
/// Every fingerprint is `SHA-256("zc-actfp-v1" || canonical_json(facts))`.
/// The tag keeps fingerprints from colliding with any other SHA-256 use, and
/// it is the evolution bump point: a changed fact schema moves to
/// `zc-actfp-v2`, which invalidates every outstanding approval at once —
/// exactly what a fact-schema change must do, since old approvals would
/// otherwise bind to facts that no longer mean the same thing.
pub const ACTION_FINGERPRINT_DOMAIN_V1: &[u8] = b"zc-actfp-v1";

/// Complete-action fingerprint: the thing an approval actually authorizes.
///
/// Computed as a domain-separated hash of the canonical serialization of the
/// action facts (see [`ActionFingerprint::compute`]). The facts are a JSON
/// object built by the tool-action extractor — for shell v1:
/// executable identity, normalized arguments, working directory, env
/// changes, redirections/stdin, and the originating principal. Matching the
/// display string is insufficient (RFC 7155 §5.2): a confirmation whose
/// fingerprint no longer matches the action at execution time is `Stale`,
/// never `Consumed`.
///
/// The type is an identifier, not a capability: constructing one from
/// arbitrary bytes authorizes nothing, because authorization happens by
/// looking the confirmation up in the runtime's ledger and comparing
/// fingerprints. That comparison is what this type makes cheap and exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionFingerprint(#[serde(with = "self::hex_serde")] pub [u8; 32]);

impl ActionFingerprint {
    /// Fingerprint the canonical serialization of the action facts.
    ///
    /// `facts` must be a JSON **object** produced by the tool-action
    /// extractor for the action being approved. Canonicality is structural:
    /// this workspace does not enable serde_json's `preserve_order`, so
    /// object keys serialize in sorted order and two `Value`s that differ
    /// only in key insertion order hash identically. Callers must not put
    /// floats or other order-sensitive scalars in the facts; the shell
    /// extractor emits strings, arrays, and objects only.
    ///
    /// Serialization of a `serde_json::Value` cannot fail; the `expect`
    /// documents that invariant rather than papering over a reachable path.
    #[must_use]
    pub fn compute(facts: &serde_json::Value) -> Self {
        use sha2::{Digest, Sha256};

        let canonical =
            serde_json::to_vec(facts).expect("serde_json::Value serialization is infallible");
        let mut hasher = Sha256::new();
        hasher.update(ACTION_FINGERPRINT_DOMAIN_V1);
        hasher.update(&canonical);
        Self(hasher.finalize().into())
    }

    /// Lowercase hex encoding, the wire/audit form (see the serde impls).
    #[must_use]
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse the lowercase-or-uppercase hex form produced by
    /// [`ActionFingerprint::as_hex`].
    ///
    /// The error is a plain message (not a typed error) because the only
    /// callers are serde deserialization and diagnostics, both of which
    /// wrap it immediately.
    pub fn from_hex(value: &str) -> Result<Self, String> {
        let bytes =
            hex::decode(value).map_err(|err| format!("invalid action fingerprint hex: {err}"))?;
        let array: [u8; 32] = bytes.try_into().map_err(|bytes: Vec<u8>| {
            format!(
                "action fingerprint must be 32 bytes, got {len}",
                len = bytes.len()
            )
        })?;
        Ok(Self(array))
    }
}

impl std::fmt::Display for ActionFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hex())
    }
}

/// Hex-string serde for [`ActionFingerprint`]: the audit log, gateway wire,
/// and channel surfaces all show fingerprints as hex, so the serialized
/// form is hex rather than a 32-number array.
mod hex_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let hex_str = <String as Deserialize>::deserialize(deserializer)?;
        let bytes = hex::decode(&hex_str).map_err(|err| {
            serde::de::Error::custom(format!("invalid action fingerprint hex: {err}"))
        })?;
        bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "action fingerprint must be 32 bytes, got {len}",
                len = bytes.len()
            ))
        })
    }
}

/// What kind of approver produced a confirmation.
///
/// v1 mints [`ApproverKind::Human`] only — automated approval is roadmap
/// Phase 5 under its own ratification — but the kind travels in the type so
/// an `Automated` confirmation from that later phase is distinguishable in
/// audit records instead of being retro-fitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApproverKind {
    /// A human answered through a trusted route.
    Human,
    /// Reserved for the separately ratified automated-approver phase
    /// (RFC 7155 §6). The v1 gate never mints one; a ledger that sees one
    /// before that phase ships is looking at a bug.
    Automated,
}

/// Which trusted route carried the approval decision.
///
/// One of: `"cli"` (the interactive CLI operator prompt), a channel alias
/// (the backchannel that actually answered a fan-out), or an
/// `approval_route` name (a routed approver profile). The route is half of
/// the confirmation's provenance — audit records answer "who approved"
/// with [`TrustedConfirmation::approver_identity`] and "through what" with
/// this id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RouteId(pub String);

impl RouteId {
    /// The interactive CLI operator prompt route.
    pub const CLI: &'static str = "cli";

    /// The interactive CLI operator prompt route.
    #[must_use]
    pub fn cli() -> Self {
        Self(Self::CLI.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RouteId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for RouteId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for RouteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The settled decision a confirmation carries.
///
/// Deliberately smaller than the channel answer surface
/// ([`crate::channel::ChannelApprovalResponse`]): `AlwaysApprove` and
/// `DenyWithEdit` are answer semantics that the approval gate resolves —
/// into a session rule or an argument replacement — *before* minting a
/// confirmation. A confirmation records only the authorize/refuse outcome
/// for the one action it fingerprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApproveOrDeny {
    /// The approver authorized this exact action.
    Approve,
    /// The approver refused this exact action.
    Deny,
}

/// Freshness window of a granted decision: when it was minted and how long
/// it stays valid.
///
/// This bounds the **decision's** lifetime, not the operator's reply time —
/// the approval wait is a separate, cancellable concern (RFC 7155 §5.4).
/// A confirmation may be consumed up to `issued_at_unix + ttl_secs`; after
/// that it is [`ConsumeOutcome::Expired`] — re-ask, not a deny. Delegation
/// may only shrink, never extend, a parent's window (RFC 7155 §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    /// When the confirmation was minted, UNIX seconds.
    pub issued_at_unix: u64,
    /// How long the granted decision stays valid, in seconds.
    pub ttl_secs: u64,
}

impl TimeWindow {
    #[must_use]
    pub fn new(issued_at_unix: u64, ttl_secs: u64) -> Self {
        Self {
            issued_at_unix,
            ttl_secs,
        }
    }

    /// Expiry deadline, saturating rather than wrapping on pathological
    /// `ttl_secs` values.
    #[must_use]
    pub fn expires_at_unix(&self) -> u64 {
        self.issued_at_unix.saturating_add(self.ttl_secs)
    }

    /// A confirmation is valid over the half-open interval
    /// `[issued_at_unix, issued_at_unix + ttl_secs)`: valid the instant it
    /// is minted, expired exactly at the deadline.
    #[must_use]
    pub fn is_valid_at(&self, now_unix: u64) -> bool {
        now_unix >= self.issued_at_unix && now_unix < self.expires_at_unix()
    }
}

/// The result of a trusted approval: authorization for exactly one action,
/// from one approver, through one trusted route, valid for one window,
/// consumable once.
///
/// Only the trusted approval path mints these — never model-supplied tool
/// arguments (RFC 7155 §5.1). The struct is `#[non_exhaustive]` so the
/// reserved extension points (authentication strength issue 3767,
/// automated-approver evidence) can be added without breaking consumers;
/// construction from outside this crate goes through
/// [`TrustedConfirmation::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TrustedConfirmation {
    /// Unique id of this confirmation. The runtime injects this id into
    /// tool arguments (replacing the legacy bool `approved` bit) so the
    /// executor can look the confirmation up in the ledger at execution
    /// time.
    pub confirmation_id: Uuid,
    /// The exact action this confirmation authorizes. Any other action is
    /// [`ConsumeOutcome::Stale`] when consumed.
    pub action_fingerprint: ActionFingerprint,
    /// The settled decision.
    pub decision: ApproveOrDeny,
    /// Freshness window. Consuming after expiry yields
    /// [`ConsumeOutcome::Expired`].
    pub validity_window: TimeWindow,
    /// What kind of approver minted this.
    pub approver_kind: ApproverKind,
    /// The trusted route the decision arrived on.
    pub trusted_route: RouteId,
    /// The authenticated principal behind the decision, when the route
    /// establishes one. `None` means the route could not attribute the
    /// answer to a principal (for example the local CLI operator prompt).
    pub approver_identity: Option<Principal>,
}

impl TrustedConfirmation {
    /// Mint a confirmation. The single construction path for callers in
    /// other crates (the struct is `#[non_exhaustive]`): the runtime
    /// approval gate calls this after a real operator/backchannel answer.
    #[must_use]
    pub fn new(
        confirmation_id: Uuid,
        action_fingerprint: ActionFingerprint,
        decision: ApproveOrDeny,
        validity_window: TimeWindow,
        approver_kind: ApproverKind,
        trusted_route: RouteId,
        approver_identity: Option<Principal>,
    ) -> Self {
        Self {
            confirmation_id,
            action_fingerprint,
            decision,
            validity_window,
            approver_kind,
            trusted_route,
            approver_identity,
        }
    }

    /// Whether the granted decision is still within its freshness window
    /// at `now_unix`. Expiry alone never denies — the caller re-asks.
    #[must_use]
    pub fn is_valid_at(&self, now_unix: u64) -> bool {
        self.validity_window.is_valid_at(now_unix)
    }
}

/// The single-use consumption result of a confirmation — the frozen
/// terminal-state table of RFC 7155 §5.2.
///
/// Every non-happy-path outcome is terminal, deterministic, and fails
/// closed: none of them silently allows execution. The table, verbatim
/// from the RFC:
///
/// | situation | outcome | effect |
/// |---|---|---|
/// | approve, first use, fingerprint matches, within window | `Consumed` | execute once |
/// | second use of an already-consumed approval | `Replay` | rejected; re-ask |
/// | response arrives after the window elapsed | `Expired` | re-ask (not a deny; not counted toward the breaker) |
/// | duplicate responses to the same request | first wins; rest are `Superseded` | no double execution |
/// | two responses disagree (approve + deny) | most-restrictive wins | `Conflicting`, fail closed as a deny |
/// | response arrives after the request was cancelled | `Cancelled` | discard; not a deny |
/// | approve whose fingerprint no longer matches the action | `Stale` | re-ask |
///
/// The variant set is deliberately **not** `#[non_exhaustive]`: this table
/// is the frozen Phase 0 contract, and a new terminal state would change
/// the contract rather than extend it — that is a v2 fingerprint-domain
/// bump, not an additive variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsumeOutcome {
    /// First use, fingerprint match, within window: execute once.
    Consumed,
    /// Second use of an already-consumed approval: rejected, re-ask.
    Replay,
    /// The validity window elapsed before consumption: re-ask. Not an
    /// operator deny, and not counted toward any breaker.
    Expired,
    /// A duplicate response after the first one won: discarded so the same
    /// request can never execute twice.
    Superseded,
    /// Approve and deny answers both arrived: the most restrictive wins,
    /// so this resolves as a deny.
    Conflicting,
    /// The request was cancelled (turn/session abort) before the answer
    /// was consumed: discard the pending confirmation. Not a deny.
    Cancelled,
    /// The confirmation's fingerprint no longer matches the action being
    /// executed: re-ask.
    Stale,
}

impl ConsumeOutcome {
    /// Only [`ConsumeOutcome::Consumed`] permits execution. Every other
    /// terminal state fails closed — that half of the table is the whole
    /// point of single-use confirmations.
    #[must_use]
    pub fn allows_execution(self) -> bool {
        matches!(self, Self::Consumed)
    }

    /// The one terminal state that resolves as a deny: when approve and
    /// deny answers conflict, the most restrictive outcome wins.
    #[must_use]
    pub fn resolves_as_deny(self) -> bool {
        matches!(self, Self::Conflicting)
    }

    /// Outcomes where the caller should re-request a fresh approval: the
    /// decision was neither refused nor made moot — it simply cannot be
    /// reused for this execution.
    #[must_use]
    pub fn requires_reask(self) -> bool {
        matches!(self, Self::Replay | Self::Expired | Self::Stale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_facts() -> serde_json::Value {
        json!({
            "executable": "rm",
            "arguments": ["-rf", "/tmp/build"],
            "cwd": "/home/op/project",
            "env_changes": [],
            "redirections": [],
            "principal": "shared-operator",
        })
    }

    // ── ActionFingerprint ─────────────────────────────────────────

    #[test]
    fn fingerprint_is_deterministic() {
        assert_eq!(
            ActionFingerprint::compute(&sample_facts()),
            ActionFingerprint::compute(&sample_facts())
        );
    }

    #[test]
    fn fingerprint_ignores_key_insertion_order() {
        // serde_json's map is BTreeMap-backed (no preserve_order in this
        // workspace), so both insertion orders canonicalize identically.
        let a = json!({"command": "ls", "cwd": "/tmp"});
        let mut map = serde_json::Map::new();
        map.insert("cwd".to_string(), json!("/tmp"));
        map.insert("command".to_string(), json!("ls"));
        let b = serde_json::Value::Object(map);
        assert_eq!(
            ActionFingerprint::compute(&a),
            ActionFingerprint::compute(&b)
        );
    }

    #[test]
    fn fingerprint_changes_when_any_fact_changes() {
        let base = sample_facts();
        for changed in [
            json!({"executable": "rmdir", "arguments": ["-rf", "/tmp/build"], "cwd": "/home/op/project", "env_changes": [], "redirections": [], "principal": "shared-operator"}),
            json!({"executable": "rm", "arguments": ["-rf", "/"], "cwd": "/home/op/project", "env_changes": [], "redirections": [], "principal": "shared-operator"}),
            json!({"executable": "rm", "arguments": ["-rf", "/tmp/build"], "cwd": "/home/op/other", "env_changes": [], "redirections": [], "principal": "shared-operator"}),
            json!({"executable": "rm", "arguments": ["-rf", "/tmp/build"], "cwd": "/home/op/project", "env_changes": [], "redirections": [], "principal": "alice"}),
        ] {
            assert_ne!(
                ActionFingerprint::compute(&base),
                ActionFingerprint::compute(&changed),
                "fingerprint must change when any fact changes"
            );
        }
    }

    #[test]
    fn fingerprint_is_domain_separated_from_raw_hash() {
        use sha2::{Digest, Sha256};

        let facts = sample_facts();
        let raw: [u8; 32] = Sha256::digest(serde_json::to_vec(&facts).unwrap()).into();
        assert_ne!(
            ActionFingerprint::compute(&facts).0,
            raw,
            "the domain tag must make fingerprints distinct from a plain hash of the facts"
        );
    }

    #[test]
    fn fingerprint_golden_vectors() {
        // SHA-256("zc-actfp-v1" || "{}") and
        // SHA-256("zc-actfp-v1" || {"command":"rm -rf /","cwd":"/tmp"}).
        // Locked at definition time: any change to the domain tag, the
        // canonicalization, or the hash choice is visible here and must be
        // a deliberate domain-version bump, never a silent drift.
        assert_eq!(
            ActionFingerprint::compute(&json!({})).as_hex(),
            "b9a6f485adc224bcaa1a9d99ed5d9538a09b81441292bc7be377106f68735b29"
        );
        assert_eq!(
            ActionFingerprint::compute(&json!({"command": "rm -rf /", "cwd": "/tmp"})).as_hex(),
            "c05b51931c32e31718f6d93a8c5417175095403e69e4676af39b5f6a8ffa3d9f"
        );
    }

    #[test]
    fn fingerprint_hex_round_trip() {
        let fingerprint = ActionFingerprint::compute(&sample_facts());
        assert_eq!(
            ActionFingerprint::from_hex(&fingerprint.as_hex()),
            Ok(fingerprint)
        );
    }

    #[test]
    fn fingerprint_from_hex_rejects_malformed_input() {
        assert!(ActionFingerprint::from_hex("not-hex").is_err());
        // 31 bytes: too short.
        assert!(ActionFingerprint::from_hex(&"0".repeat(62)).is_err());
        // 33 bytes: too long.
        assert!(ActionFingerprint::from_hex(&"0".repeat(66)).is_err());
    }

    #[test]
    fn fingerprint_serde_round_trip_is_hex_string() {
        let fingerprint = ActionFingerprint::compute(&sample_facts());
        let serialized = serde_json::to_string(&fingerprint).unwrap();
        assert_eq!(serialized, format!("\"{}\"", fingerprint.as_hex()));
        let parsed: ActionFingerprint = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, fingerprint);
    }

    #[test]
    fn fingerprint_display_is_hex() {
        let fingerprint = ActionFingerprint::compute(&sample_facts());
        assert_eq!(fingerprint.to_string(), fingerprint.as_hex());
    }

    // ── TimeWindow ────────────────────────────────────────────────

    #[test]
    fn time_window_is_valid_at_issuance_instant() {
        let window = TimeWindow::new(1_000, 300);
        assert!(window.is_valid_at(1_000));
        assert!(window.is_valid_at(1_299));
    }

    #[test]
    fn time_window_expires_exactly_at_deadline() {
        // Half-open [issued, issued+ttl): the deadline instant itself is
        // expired, so a consume landing exactly on the boundary re-asks.
        let window = TimeWindow::new(1_000, 300);
        assert!(!window.is_valid_at(1_300));
    }

    #[test]
    fn time_window_invalid_before_issuance() {
        let window = TimeWindow::new(1_000, 300);
        assert!(!window.is_valid_at(999));
    }

    #[test]
    fn time_window_deadline_saturates_instead_of_wrapping() {
        let window = TimeWindow::new(u64::MAX, 1);
        assert_eq!(window.expires_at_unix(), u64::MAX);
    }

    // ── ConsumeOutcome ────────────────────────────────────────────

    #[test]
    fn only_consumed_allows_execution() {
        for outcome in [
            ConsumeOutcome::Replay,
            ConsumeOutcome::Expired,
            ConsumeOutcome::Superseded,
            ConsumeOutcome::Conflicting,
            ConsumeOutcome::Cancelled,
            ConsumeOutcome::Stale,
        ] {
            assert!(!outcome.allows_execution(), "{outcome:?} must fail closed");
        }
        assert!(ConsumeOutcome::Consumed.allows_execution());
    }

    #[test]
    fn only_conflicting_resolves_as_deny() {
        for outcome in [
            ConsumeOutcome::Consumed,
            ConsumeOutcome::Replay,
            ConsumeOutcome::Expired,
            ConsumeOutcome::Superseded,
            ConsumeOutcome::Cancelled,
            ConsumeOutcome::Stale,
        ] {
            assert!(!outcome.resolves_as_deny(), "{outcome:?} is not a deny");
        }
        assert!(ConsumeOutcome::Conflicting.resolves_as_deny());
    }

    #[test]
    fn replay_expired_and_stale_require_reask() {
        for outcome in [
            ConsumeOutcome::Replay,
            ConsumeOutcome::Expired,
            ConsumeOutcome::Stale,
        ] {
            assert!(outcome.requires_reask(), "{outcome:?} should re-ask");
        }
        for outcome in [
            ConsumeOutcome::Consumed,
            ConsumeOutcome::Superseded,
            ConsumeOutcome::Conflicting,
            ConsumeOutcome::Cancelled,
        ] {
            assert!(
                !outcome.requires_reask(),
                "{outcome:?} must not re-ask (consumed executes; superseded/cancelled are discarded; conflicting denies)"
            );
        }
    }

    #[test]
    fn consume_outcome_serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&ConsumeOutcome::Consumed).unwrap(),
            "\"consumed\""
        );
        let parsed: ConsumeOutcome = serde_json::from_str("\"expired\"").unwrap();
        assert_eq!(parsed, ConsumeOutcome::Expired);
    }

    // ── TrustedConfirmation ───────────────────────────────────────

    fn sample_confirmation() -> TrustedConfirmation {
        TrustedConfirmation::new(
            Uuid::nil(),
            ActionFingerprint::compute(&sample_facts()),
            ApproveOrDeny::Approve,
            TimeWindow::new(1_000, 300),
            ApproverKind::Human,
            RouteId::cli(),
            None,
        )
    }

    #[test]
    fn confirmation_delegates_validity_to_window() {
        let confirmation = sample_confirmation();
        assert!(confirmation.is_valid_at(1_200));
        assert!(!confirmation.is_valid_at(1_300));
    }

    #[test]
    fn confirmation_serde_round_trip() {
        let confirmation = sample_confirmation();
        let serialized = serde_json::to_string(&confirmation).unwrap();
        let parsed: TrustedConfirmation = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, confirmation);
    }

    #[test]
    fn confirmation_serde_shape_is_stable() {
        // The wire shape is a frozen cross-surface contract: fingerprint as
        // hex, enums as lowercase snake_case, uuid as canonical string.
        let confirmation = sample_confirmation();
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&confirmation).unwrap()).unwrap();
        assert_eq!(
            value["confirmation_id"],
            serde_json::json!("00000000-0000-0000-0000-000000000000")
        );
        assert_eq!(value["decision"], serde_json::json!("approve"));
        assert_eq!(
            value["validity_window"]["issued_at_unix"],
            serde_json::json!(1_000)
        );
        assert_eq!(value["validity_window"]["ttl_secs"], serde_json::json!(300));
        assert_eq!(value["approver_kind"], serde_json::json!("human"));
        assert_eq!(value["trusted_route"], serde_json::json!("cli"));
        assert_eq!(value["approver_identity"], serde_json::json!(null));
        assert_eq!(
            value["action_fingerprint"],
            serde_json::json!(ActionFingerprint::compute(&sample_facts()).as_hex())
        );
    }

    #[test]
    fn confirmation_carries_approver_principal_when_route_establishes_one() {
        let confirmation = TrustedConfirmation::new(
            Uuid::nil(),
            ActionFingerprint::compute(&sample_facts()),
            ApproveOrDeny::Deny,
            TimeWindow::new(1_000, 300),
            ApproverKind::Human,
            RouteId::from("telegram.ops"),
            Some(Principal::shared_operator()),
        );
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&confirmation).unwrap()).unwrap();
        assert_eq!(value["trusted_route"], serde_json::json!("telegram.ops"));
        assert_eq!(value["decision"], serde_json::json!("deny"));
        assert_eq!(
            value["approver_identity"]["id"],
            serde_json::json!("shared-operator")
        );
    }

    // ── RouteId ───────────────────────────────────────────────────

    #[test]
    fn route_id_cli_is_the_documented_constant() {
        assert_eq!(RouteId::cli().as_str(), "cli");
        assert_eq!(RouteId::CLI, "cli");
    }

    #[test]
    fn route_id_from_string_conversions() {
        assert_eq!(RouteId::from("slack.ops").as_str(), "slack.ops");
        assert_eq!(
            RouteId::from(String::from("approval_route.europe")).0,
            "approval_route.europe"
        );
        assert_eq!(RouteId::from("cli").to_string(), "cli");
    }
}
