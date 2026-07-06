use serde::{Deserialize, Serialize};

/// How much autonomy the agent has.
///
/// Variants are ordered from least to most autonomous so that
/// [`Ord`] / [`PartialOrd`] compare a child's level against a
/// parent's during SubAgent escalation checks (`child <= parent`).
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    zeroclaw_macros::ConfigEnum,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    /// Read-only: can observe but not act
    ReadOnly,
    /// Supervised: acts but requires approval for risky operations
    #[default]
    Supervised,
    /// Full: autonomous execution within policy bounds
    Full,
}

/// Delegation mode for a risk profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum DelegationMode {
    /// No delegation permitted.
    #[default]
    Forbidden,
    /// Delegation permitted to the agents named in the allow-list.
    Allow,
}

impl crate::config::HasPropKind for DelegationMode {
    const PROP_KIND: crate::config::PropKind = crate::config::PropKind::Enum;
}

/// Whether a risk profile may delegate work to other agents.
///
/// `Forbidden` (the default) means a profile cannot delegate at all; `Allow`
/// permits delegation. The set of reachable targets is *not* an explicit
/// allow-list — delegation is gated on the caller and target sharing a risk
/// profile, so the shared profile determines who is reachable.
///
/// Wire format: `{ mode = "forbidden" }` or `{ mode = "allow" }`. The struct
/// shape lets the prop layer expose `mode` as an editable enum leaf.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, zeroclaw_macros::Configurable,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DelegationPolicy {
    #[serde(default)]
    pub mode: DelegationMode,
}

impl DelegationPolicy {
    /// Whether this profile may delegate. The set of reachable targets is
    /// determined by shared risk profile at the call site — this only gates
    /// whether delegation is permitted at all.
    pub fn permits(&self) -> bool {
        matches!(self.mode, DelegationMode::Allow)
    }
}

/// What to do when a configured approver cannot be reached. Default FAIL-CLOSED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum OnNoApprover {
    /// Fail-closed: deny when the approver is unreachable / declines / times out.
    #[default]
    Deny,
    /// Explicit opt-in: fall back to the originating channel (today's behavior).
    InheritOriginator,
}

impl crate::config::HasPropKind for OnNoApprover {
    const PROP_KIND: crate::config::PropKind = crate::config::PropKind::Enum;
}

fn default_approval_timeout_secs() -> u64 {
    120
}

/// Route a risk profile's tool approvals to a DISTINCT approver channel instead of the
/// channel that triggered the run — closing the cross-channel-HITL gap (an agent's gated
/// actions can be approved by a separate ops channel / a different principal).
///
/// `Option<ApprovalRoute>` on a risk profile: ABSENT ⇒ today's behavior (the originating
/// channel approves). Present ⇒ the gate asks `approver_channel` (a channel registry key,
/// platform-qualified `<channel>.<alias>` such as `matrix.ops`, NOT the originator),
/// bounded by `timeout_secs`, fail-closed by default.
///
/// Consulted on both the interactive channel-driven path and the non-interactive turn path
/// (gateway chat/webhook dispatch and agent-to-agent peer messages, which run without an
/// originating channel). The non-interactive path resolves `approver_channel` from the live
/// daemon channel registry; with no live registry/approver it keeps the non-interactive
/// default (fail-closed deny under the default `on_no_approver`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ApprovalRoute {
    /// A registered channel name (NOT the originator) — the distinct-approver hop.
    pub approver_channel: String,
    /// Behavior when the approver is unreachable. Fail-closed by default.
    #[serde(default)]
    pub on_no_approver: OnNoApprover,
    /// Bound the approver's response window; a timeout denies (DoS guard). Default 120s.
    #[serde(default = "default_approval_timeout_secs")]
    pub timeout_secs: u64,
}

impl crate::config::HasPropKind for ApprovalRoute {
    const PROP_KIND: crate::config::PropKind = crate::config::PropKind::Object;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_default_is_forbidden() {
        assert_eq!(DelegationPolicy::default().mode, DelegationMode::Forbidden);
        assert!(!DelegationPolicy::default().permits());
    }

    #[test]
    fn delegation_allow_permits() {
        let p = DelegationPolicy {
            mode: DelegationMode::Allow,
        };
        assert!(p.permits());
        assert!(!DelegationPolicy::default().permits());
    }

    #[test]
    fn approval_route_defaults_are_fail_closed() {
        // Absent optional fields: fail-closed policy + bounded 120s window.
        let r: ApprovalRoute = toml::from_str("approver_channel = \"ops\"").unwrap();
        assert_eq!(r.approver_channel, "ops");
        assert_eq!(
            r.on_no_approver,
            OnNoApprover::Deny,
            "default must fail closed"
        );
        assert_eq!(r.timeout_secs, 120);
    }

    #[test]
    fn approval_route_round_trips() {
        let r = ApprovalRoute {
            approver_channel: "ops".into(),
            on_no_approver: OnNoApprover::InheritOriginator,
            timeout_secs: 30,
        };
        let s = toml::to_string(&r).unwrap();
        // kebab-case enum on the wire.
        assert!(s.contains("on_no_approver = \"inherit-originator\""), "{s}");
        let back: ApprovalRoute = toml::from_str(&s).unwrap();
        assert_eq!(back.approver_channel, r.approver_channel);
        assert_eq!(back.on_no_approver, r.on_no_approver);
        assert_eq!(back.timeout_secs, r.timeout_secs);
    }

    #[test]
    fn risk_profile_has_no_route_by_default() {
        use crate::schema::RiskProfileConfig;
        assert!(
            RiskProfileConfig::default().approval_route.is_none(),
            "default profile must keep today's originating-channel behavior"
        );
    }

    #[test]
    fn delegation_wire_format() {
        // Forbidden serializes to `{ mode = "forbidden" }`.
        let forbidden = toml::to_string(&DelegationPolicy::default()).unwrap();
        assert!(forbidden.contains("mode = \"forbidden\""), "{forbidden}");

        // Allow round-trips `{ mode = "allow" }`.
        let allow = DelegationPolicy {
            mode: DelegationMode::Allow,
        };
        let s = toml::to_string(&allow).unwrap();
        assert!(s.contains("mode = \"allow\""), "{s}");
        let back: DelegationPolicy = toml::from_str(&s).unwrap();
        assert_eq!(back, allow);
    }
}

#[cfg(test)]
mod prop_exposure_tests {
    use crate::schema::RiskProfileConfig;
    use crate::traits::PropKind;

    #[test]
    fn delegation_policy_exposes_mode_enum_leaf() {
        let p = RiskProfileConfig::default();
        let mode = p
            .prop_fields()
            .into_iter()
            .find(|f| f.name.ends_with("delegation_policy.mode"))
            .expect("delegation_policy.mode leaf missing");
        assert_eq!(mode.kind, PropKind::Enum);
    }
}

#[cfg(all(test, feature = "schema-export"))]
mod enum_variant_tests {
    use super::DelegationMode;
    use crate::schema::RiskProfileConfig;

    #[test]
    fn delegation_mode_variants_surface() {
        let v = crate::helpers::enum_variants::<DelegationMode>();
        assert!(v.contains("forbidden"), "{v}");
        assert!(v.contains("allow"), "{v}");
    }

    #[test]
    fn delegation_mode_field_carries_variants() {
        let p = RiskProfileConfig::default();
        let mode = p
            .prop_fields()
            .into_iter()
            .find(|f| f.name.ends_with("delegation_policy.mode"))
            .expect("mode leaf missing");
        let variants = mode.enum_variants.map(|f| f()).unwrap_or_default();
        assert!(
            !variants.is_empty(),
            "enum_variants empty — UI would render as text"
        );
        assert!(variants.iter().any(|v| v == "forbidden"));
        assert!(variants.iter().any(|v| v == "allow"));
    }
}
