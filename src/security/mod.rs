pub mod audit;
#[cfg(feature = "sandbox-bubblewrap")]
pub mod bubblewrap;
pub mod detect;
pub mod docker;
#[cfg(target_os = "linux")]
pub mod firejail;
#[cfg(feature = "sandbox-landlock")]
pub mod landlock;
pub mod pairing;
pub mod policy;
pub mod resource_limits;
pub mod secrets;
pub mod traits;

pub use audit::AuditLogger;
#[allow(unused_imports)]
pub use audit::{AuditEvent, AuditEventType};
pub use detect::create_sandbox;
#[allow(unused_imports)]
pub use pairing::PairingGuard;
pub use policy::{AutonomyLevel, SecurityPolicy};
#[allow(unused_imports)]
pub use resource_limits::ResourceLimits;
pub use secrets::SecretStore;
#[allow(unused_imports)]
pub use traits::NoopSandbox;
pub use traits::Sandbox;

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
}
