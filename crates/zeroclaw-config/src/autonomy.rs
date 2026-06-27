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
