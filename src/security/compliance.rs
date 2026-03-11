//! Compliance framework classification and data residency enforcement.
//!
//! Provides regulatory classification for agent actions (FINMA, DORA, GDPR,
//! SOC2, ISO27001) and data residency policy enforcement that can block
//! actions violating geographic constraints.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Supported regulatory compliance frameworks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComplianceFramework {
    /// Swiss Financial Market Supervisory Authority
    Finma,
    /// Digital Operational Resilience Act (EU)
    Dora,
    /// General Data Protection Regulation (EU)
    Gdpr,
    /// Service Organization Control 2
    Soc2,
    /// Information security management standard
    Iso27001,
    /// User-defined framework label
    Custom(String),
}

impl ComplianceFramework {
    /// Parse a framework name string into the enum variant.
    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_uppercase().as_str() {
            "FINMA" => Self::Finma,
            "DORA" => Self::Dora,
            "GDPR" => Self::Gdpr,
            "SOC2" => Self::Soc2,
            "ISO27001" => Self::Iso27001,
            // Preserve original case for custom framework labels.
            _ => Self::Custom(name.to_string()),
        }
    }

    /// Canonical string label for this framework.
    pub fn label(&self) -> &str {
        match self {
            Self::Finma => "FINMA",
            Self::Dora => "DORA",
            Self::Gdpr => "GDPR",
            Self::Soc2 => "SOC2",
            Self::Iso27001 => "ISO27001",
            Self::Custom(s) => s.as_str(),
        }
    }
}

/// Tags an action with the regulatory frameworks it is relevant to.
pub struct ComplianceClassifier {
    active_frameworks: HashSet<ComplianceFramework>,
}

impl ComplianceClassifier {
    /// Create a classifier for the given active frameworks.
    pub fn new(frameworks: &[ComplianceFramework]) -> Self {
        Self {
            active_frameworks: frameworks.iter().cloned().collect(),
        }
    }

    /// Classify an action string and return the set of relevant framework tags.
    ///
    /// Classification is keyword-based: actions touching data, access control,
    /// encryption, or financial operations are tagged with the relevant frameworks.
    pub fn classify(&self, action: &str) -> Vec<ComplianceFramework> {
        let lower = action.to_ascii_lowercase();
        let mut tags = Vec::new();

        // GDPR: personal data, consent, erasure
        if self.active_frameworks.contains(&ComplianceFramework::Gdpr)
            && (lower.contains("personal_data")
                || lower.contains("pii")
                || lower.contains("consent")
                || lower.contains("erasure")
                || lower.contains("data_subject")
                || lower.contains("gdpr"))
        {
            tags.push(ComplianceFramework::Gdpr);
        }

        // FINMA: financial transactions, account access, reporting
        if self.active_frameworks.contains(&ComplianceFramework::Finma)
            && (lower.contains("transaction")
                || lower.contains("account")
                || lower.contains("financial")
                || lower.contains("payment")
                || lower.contains("finma")
                || lower.contains("kyc")
                || lower.contains("aml"))
        {
            tags.push(ComplianceFramework::Finma);
        }

        // DORA: ICT risk, resilience, incident reporting
        if self.active_frameworks.contains(&ComplianceFramework::Dora)
            && (lower.contains("ict_risk")
                || lower.contains("incident")
                || lower.contains("resilience")
                || lower.contains("third_party")
                || lower.contains("dora")
                || lower.contains("continuity"))
        {
            tags.push(ComplianceFramework::Dora);
        }

        // SOC2: security controls, availability, confidentiality
        if self.active_frameworks.contains(&ComplianceFramework::Soc2)
            && (lower.contains("access_control")
                || lower.contains("encryption")
                || lower.contains("availability")
                || lower.contains("confidential")
                || lower.contains("soc2")
                || lower.contains("audit"))
        {
            tags.push(ComplianceFramework::Soc2);
        }

        // ISO27001: information security management
        if self
            .active_frameworks
            .contains(&ComplianceFramework::Iso27001)
            && (lower.contains("security_policy")
                || lower.contains("risk_assessment")
                || lower.contains("asset_management")
                || lower.contains("iso27001")
                || lower.contains("information_security"))
        {
            tags.push(ComplianceFramework::Iso27001);
        }

        tags
    }
}

/// Enforcement mode for data residency violations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidencyEnforcementMode {
    /// Block the action entirely.
    #[default]
    Block,
    /// Log a warning but allow the action.
    Warn,
}

/// Data classification level for residency policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClassification {
    Public,
    Internal,
    Confidential,
    Restricted,
}

/// Defines data residency constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataResidencyPolicy {
    /// ISO 3166-1 alpha-2 region codes where data may reside.
    pub allowed_regions: Vec<String>,
    /// Classification level of data this policy covers.
    pub data_classification: DataClassification,
    /// What to do when a violation is detected.
    #[serde(default)]
    pub enforcement_mode: ResidencyEnforcementMode,
}

/// Result of a residency check.
#[derive(Debug, Clone)]
pub enum ResidencyCheckResult {
    /// Action is allowed (region is in the allowed set).
    Allowed,
    /// Action is blocked due to residency violation.
    Blocked {
        region: String,
        allowed: Vec<String>,
    },
    /// Action is warned but not blocked.
    Warned {
        region: String,
        allowed: Vec<String>,
    },
}

impl DataResidencyPolicy {
    /// Check whether a target region is allowed by this policy.
    pub fn check_region(&self, target_region: &str) -> ResidencyCheckResult {
        let normalized = target_region.to_ascii_uppercase();
        let allowed_upper: Vec<String> = self
            .allowed_regions
            .iter()
            .map(|r| r.to_ascii_uppercase())
            .collect();

        if allowed_upper.contains(&normalized) {
            return ResidencyCheckResult::Allowed;
        }

        match self.enforcement_mode {
            ResidencyEnforcementMode::Block => ResidencyCheckResult::Blocked {
                region: normalized,
                allowed: allowed_upper,
            },
            ResidencyEnforcementMode::Warn => ResidencyCheckResult::Warned {
                region: normalized,
                allowed: allowed_upper,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_from_name_roundtrip() {
        assert_eq!(
            ComplianceFramework::from_name("finma"),
            ComplianceFramework::Finma
        );
        assert_eq!(
            ComplianceFramework::from_name("GDPR"),
            ComplianceFramework::Gdpr
        );
        // Custom frameworks preserve original case
        assert_eq!(
            ComplianceFramework::from_name("CUSTOM_RULE"),
            ComplianceFramework::Custom("CUSTOM_RULE".to_string())
        );
        assert_eq!(
            ComplianceFramework::from_name("My_Custom_Rule"),
            ComplianceFramework::Custom("My_Custom_Rule".to_string())
        );
    }

    #[test]
    fn classifier_tags_gdpr_action() {
        let classifier = ComplianceClassifier::new(&[ComplianceFramework::Gdpr]);
        let tags = classifier.classify("process_personal_data_erasure");
        assert!(tags.contains(&ComplianceFramework::Gdpr));
    }

    #[test]
    fn classifier_tags_finma_action() {
        let classifier =
            ComplianceClassifier::new(&[ComplianceFramework::Finma, ComplianceFramework::Gdpr]);
        let tags = classifier.classify("submit_financial_transaction");
        assert!(tags.contains(&ComplianceFramework::Finma));
        assert!(!tags.contains(&ComplianceFramework::Gdpr));
    }

    #[test]
    fn classifier_returns_empty_for_unrelated_action() {
        let classifier =
            ComplianceClassifier::new(&[ComplianceFramework::Gdpr, ComplianceFramework::Finma]);
        let tags = classifier.classify("list_directory_contents");
        assert!(tags.is_empty());
    }

    #[test]
    fn residency_allowed_region() {
        let policy = DataResidencyPolicy {
            allowed_regions: vec!["CH".to_string(), "EU".to_string()],
            data_classification: DataClassification::Confidential,
            enforcement_mode: ResidencyEnforcementMode::Block,
        };
        assert!(matches!(
            policy.check_region("CH"),
            ResidencyCheckResult::Allowed
        ));
        assert!(matches!(
            policy.check_region("ch"),
            ResidencyCheckResult::Allowed
        ));
    }

    #[test]
    fn residency_blocked_region() {
        let policy = DataResidencyPolicy {
            allowed_regions: vec!["CH".to_string()],
            data_classification: DataClassification::Restricted,
            enforcement_mode: ResidencyEnforcementMode::Block,
        };
        assert!(matches!(
            policy.check_region("US"),
            ResidencyCheckResult::Blocked { .. }
        ));
    }

    #[test]
    fn residency_warned_region() {
        let policy = DataResidencyPolicy {
            allowed_regions: vec!["CH".to_string()],
            data_classification: DataClassification::Internal,
            enforcement_mode: ResidencyEnforcementMode::Warn,
        };
        assert!(matches!(
            policy.check_region("US"),
            ResidencyCheckResult::Warned { .. }
        ));
    }
}
