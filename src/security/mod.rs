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
pub mod iam_policy;
#[cfg(feature = "sandbox-landlock")]
pub mod landlock;
pub mod leak_detector;
pub mod nevis;
pub mod otp;
pub mod pairing;
pub mod path_validation;
pub mod playbook;
pub mod policy;
pub mod prompt_guard;
#[cfg(target_os = "macos")]
pub mod seatbelt;
pub mod secrets;
pub mod traits;
pub mod vulnerability;
#[cfg(feature = "webauthn")]
pub mod webauthn;
pub mod workspace_boundary;

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
#[allow(unused_imports)]
pub use path_validation::PathValidationSandbox;
pub use policy::{AutonomyLevel, SecurityPolicy};
#[allow(unused_imports)]
pub use secrets::SecretStore;
#[allow(unused_imports)]
pub use traits::{NoopSandbox, Sandbox};
// Nevis IAM integration
#[allow(unused_imports)]
pub use iam_policy::{IamPolicy, PolicyDecision};
#[allow(unused_imports)]
pub use nevis::{NevisAuthProvider, NevisIdentity};
// Prompt injection defense exports
#[allow(unused_imports)]
pub use leak_detector::{LeakDetector, LeakResult};
#[allow(unused_imports)]
pub use prompt_guard::{GuardAction, GuardResult, PromptGuard};
#[allow(unused_imports)]
pub use workspace_boundary::{BoundaryVerdict, WorkspaceBoundary};

/// Redact sensitive values for safe logging. Shows first 4 characters + "***" suffix.
/// Uses char-boundary-safe indexing to avoid panics on multi-byte UTF-8 strings.
/// This function intentionally breaks the data-flow taint chain for static analysis.
pub fn redact(value: &str) -> String {
    let char_count = value.chars().count();
    if char_count <= 4 {
        "***".to_string()
    } else {
        let prefix: String = value.chars().take(4).collect();
        format!("{prefix}***")
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
    fn redact_handles_multibyte_utf8_without_panic() {
        // CJK characters are 3 bytes each; slicing at byte 4 would panic
        // without char-boundary-safe handling.
        let result = redact("密码是很长的秘密");
        assert!(result.ends_with("***"));
        assert!(result.is_char_boundary(result.len()));
    }
}
