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
#[cfg(feature = "sandbox-bubblewrap")]
pub mod bubblewrap;
pub mod detect;
pub mod docker;

// Prompt injection defense (contributed from RustyClaw, MIT licensed)
pub mod domain_matcher;
pub mod estop;
#[cfg(target_os = "linux")]
pub mod firejail;
#[cfg(feature = "sandbox-landlock")]
pub mod landlock;
pub mod leak_detector;
pub mod otp;
pub mod pairing;
pub mod policy;
pub mod prompt_guard;
pub mod secrets;
pub mod traits;

#[allow(unused_imports)]
pub use audit::{AuditEvent, AuditEventType, AuditLogger};
#[allow(unused_imports)]
pub use detect::create_sandbox;
pub use domain_matcher::DomainMatcher;
#[allow(unused_imports)]
pub use estop::{EstopLevel, EstopManager, EstopState, ResumeSelector};
#[allow(unused_imports)]
pub use otp::OtpValidator;
#[allow(unused_imports)]
pub use pairing::PairingGuard;
pub use policy::{AutonomyLevel, SecurityPolicy};
#[allow(unused_imports)]
pub use secrets::SecretStore;
#[allow(unused_imports)]
pub use traits::{NoopSandbox, Sandbox};
// Prompt injection defense exports
pub use leak_detector::{LeakDetector, LeakResult};
#[allow(unused_imports)]
pub use prompt_guard::{GuardAction, GuardResult, PromptGuard};

/// Apply output guardrails to content before it reaches any output channel.
///
/// Currently performs credential leak detection via [`LeakDetector`]. Scans for
/// API keys, AWS credentials, JWTs, PEM private keys, database URLs, and
/// generic secret patterns. The `sensitivity` field only affects heuristic
/// rules (generic passwords/secrets/tokens); structurally identifiable patterns
/// (Stripe, OpenAI, Anthropic, GitHub, AWS, JWT, PEM, DB URLs) are always
/// detected regardless of sensitivity.
///
/// Future phases will add prompt-injection scanning and user-extensible
/// guardrail traits.
pub fn apply_output_guardrail(
    content: &str,
    config: &crate::config::OutputGuardrailConfig,
) -> String {
    if !config.leak_detection {
        return content.to_string();
    }

    let detector = LeakDetector::with_sensitivity(config.leak_sensitivity);
    match detector.scan(content) {
        LeakResult::Clean => content.to_string(),
        LeakResult::Detected { patterns, redacted } => {
            tracing::warn!(
                detected_patterns = ?patterns,
                "output guardrail: credential leak detected in outbound message"
            );
            match config.leak_action {
                crate::config::LeakAction::Redact => redacted,
                crate::config::LeakAction::Warn => content.to_string(),
                crate::config::LeakAction::Block => format!(
                    "⚠️ Response blocked: potential credential leak detected ({}).",
                    patterns.join(", ")
                ),
            }
        }
    }
}

/// Validate shell environment variable names (`[A-Za-z_][A-Za-z0-9_]*`).
pub fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
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
}
