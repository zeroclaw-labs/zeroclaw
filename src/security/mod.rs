//! Security subsystem for policy enforcement, sandboxing, and secret management.
//!
//! This module provides the security infrastructure for ZeroClaw. The core type
//! [`SecurityPolicy`] defines autonomy levels, workspace boundaries, and
//! access-control rules that are enforced across the tool and runtime subsystems.
//! [`PairingGuard`] implements device pairing for channel authentication, and
//! [`SecretStore`] handles encrypted credential storage.
//!
//! OS-level isolation is provided through the [`Sandbox`] trait defined in
//! [`traits`], with pluggable backends including Docker, Firejail, Bubblewrap,
//! and Landlock. The [`create_sandbox`] function selects the best available
//! backend at runtime. An [`AuditLogger`] records security-relevant events for
//! forensic review.
//!
//! # Extension
//!
//! To add a new sandbox backend, implement [`Sandbox`] in a new submodule and
//! register it in [`detect::create_sandbox`]. See `AGENTS.md` §7.5 for security
//! change guidelines.

pub mod audit;
pub mod auto_trigger;
#[cfg(feature = "sandbox-bubblewrap")]
pub mod bubblewrap;
pub mod detect;
pub mod docker;
pub mod domain_matcher;
pub mod estop;
#[cfg(target_os = "linux")]
pub mod firejail;
#[cfg(feature = "sandbox-landlock")]
pub mod landlock;
pub mod otp;
pub mod otp_prompt;
pub mod pairing;
pub mod policy;
pub mod secrets;
pub mod traits;

use serde::{Deserialize, Serialize};

#[allow(unused_imports)]
pub use audit::{AuditEvent, AuditEventType, AuditLogger};
#[allow(unused_imports)]
pub use auto_trigger::{AutoTriggerDecision, AutoTriggerEngine, AutoTriggerType};
#[allow(unused_imports)]
pub use detect::create_sandbox;
pub use domain_matcher::DomainMatcher;
#[allow(unused_imports)]
pub use estop::{EstopLevel, EstopLoadStatus, EstopManager, EstopState, ResumeSelector};
#[allow(unused_imports)]
pub use otp::{OtpApprovalCache, OtpValidator};
#[allow(unused_imports)]
pub use otp_prompt::{OtpApproved, OtpDenied, OtpPending, OtpPromptHandler};
#[allow(unused_imports)]
pub use pairing::PairingGuard;
pub use policy::{AutonomyLevel, SecurityPolicy};
#[allow(unused_imports)]
pub use secrets::SecretStore;
#[allow(unused_imports)]
pub use traits::{NoopSandbox, Sandbox};

/// Scope that triggered an OTP challenge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OtpRequiredScope {
    Tool,
    Domain,
}

/// Structured response emitted when a tool call requires OTP verification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OtpRequired {
    #[serde(rename = "type")]
    pub response_type: String,
    pub scope: OtpRequiredScope,
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub parameters_summary: String,
    pub prompt: String,
}

impl OtpRequired {
    pub fn for_tool(tool_name: impl Into<String>, parameters_summary: impl Into<String>) -> Self {
        let tool_name = tool_name.into();
        Self {
            response_type: "otp_required".to_string(),
            scope: OtpRequiredScope::Tool,
            prompt: format!("OTP required for {tool_name}. Enter code to continue."),
            tool_name,
            domain: None,
            parameters_summary: parameters_summary.into(),
        }
    }

    pub fn for_domain(
        tool_name: impl Into<String>,
        domain: impl Into<String>,
        parameters_summary: impl Into<String>,
    ) -> Self {
        let tool_name = tool_name.into();
        let domain = domain.into();
        Self {
            response_type: "otp_required".to_string(),
            scope: OtpRequiredScope::Domain,
            prompt: format!(
                "OTP required for {tool_name} on domain '{domain}'. Enter code to continue."
            ),
            tool_name,
            domain: Some(domain),
            parameters_summary: parameters_summary.into(),
        }
    }
}

/// Redact sensitive values for safe logging. Shows first 4 chars + "***" suffix.
/// This function intentionally breaks the data-flow taint chain for static analysis.
pub fn redact(value: &str) -> String {
    if value.len() <= 4 {
        "***".to_string()
    } else {
        format!("{}***", &value[..4])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexported_policy_and_pairing_types_are_usable() {
        let policy = SecurityPolicy::default();
        assert_eq!(policy.autonomy, AutonomyLevel::Supervised);

        let guard = PairingGuard::new(false, &[]);
        assert!(!guard.require_pairing());
    }

    #[test]
    fn reexported_secret_store_encrypt_decrypt_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let store = SecretStore::new(temp.path(), false);

        let encrypted = store.encrypt("top-secret").unwrap();
        let decrypted = store.decrypt(&encrypted).unwrap();

        assert_eq!(decrypted, "top-secret");
    }

    #[test]
    fn redact_hides_most_of_value() {
        assert_eq!(redact("abcdefgh"), "abcd***");
        assert_eq!(redact("ab"), "***");
        assert_eq!(redact(""), "***");
        assert_eq!(redact("12345"), "1234***");
    }

    #[test]
    fn otp_required_serialization_is_stable() {
        let payload = OtpRequired::for_domain("browser_open", "chase.com", "url=https://...");
        let as_json = serde_json::to_value(payload).unwrap();
        assert_eq!(as_json["type"], "otp_required");
        assert_eq!(as_json["scope"], "domain");
        assert_eq!(as_json["tool_name"], "browser_open");
        assert_eq!(as_json["domain"], "chase.com");
    }
}
