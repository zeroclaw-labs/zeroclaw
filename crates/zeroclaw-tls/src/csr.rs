//! Platform-abstracted client CSR generation.
//!
//! A client's inner-mTLS private key SHOULD be non-exportable and hardware-backed
//! (iOS Secure Enclave / Android Keystore) so it cannot be exfiltrated even if the
//! device is compromised (threat A5). Those keystores expose only ECDSA P-256, so
//! that is the default client suite; the CA signs P-256 leaves and mixed-algorithm
//! chains are fine in rustls/webpki.
//!
//! This module is the seam. [`CsrSigner`] produces a PKCS#10 CSR while the private
//! key stays wherever the platform holds it. The in-tree [`SoftwareP256Signer`] is
//! the desktop path: an extractable rcgen P-256 key the caller persists locally.
//! Mobile clients implement [`CsrSigner`] against their keystore and return a
//! [`ClientKey::HardwareAlias`]. In either case only the CSR ever leaves the
//! device; the issuer overrides the requested subject with the device id it
//! assigns (A7), so a client cannot choose its own identity.

use anyhow::Result;
use zeroize::Zeroizing;

/// Where a freshly generated client private key lives after CSR creation.
pub enum ClientKey {
    /// An extractable PKCS#8 PEM key (desktop / software path). The caller writes
    /// it to `client.key` at `0600`. Wrapped in [`Zeroizing`] so the in-memory
    /// copy is wiped on drop.
    Software(Zeroizing<String>),
    /// An opaque handle to a non-exportable key held in a platform hardware
    /// keystore (Secure Enclave / Android Keystore). The key never leaves the
    /// device; only this alias is recorded. Produced by mobile signers.
    HardwareAlias(String),
}

/// A generated client CSR plus the handle to its private key.
pub struct ClientCsr {
    /// PEM-encoded PKCS#10 certificate signing request to send to the issuer.
    pub csr_pem: String,
    /// The private key handle (never transmitted).
    pub key: ClientKey,
}

/// Produces a client CSR while holding the private key per the platform.
///
/// Desktop uses [`SoftwareP256Signer`]; mobile clients implement this against a
/// hardware keystore so the key is non-exportable (A5). Implementations MUST NOT
/// return the private key in the CSR or transmit it off-device.
pub trait CsrSigner {
    /// Generate a keypair and a CSR for `subject_hint`. The hint is advisory: the
    /// issuer overrides the subject with the device id it assigns (A7).
    fn generate_csr(&self, subject_hint: &str) -> Result<ClientCsr>;
}

/// The desktop signer: an extractable ECDSA P-256 keypair generated in software
/// via rcgen. The key is returned to the caller to persist locally.
#[derive(Debug, Default, Clone, Copy)]
pub struct SoftwareP256Signer;

impl CsrSigner for SoftwareP256Signer {
    fn generate_csr(&self, subject_hint: &str) -> Result<ClientCsr> {
        let (csr_pem, key_pem) = crate::generate_client_csr(subject_hint)?;
        Ok(ClientCsr {
            csr_pem,
            key: ClientKey::Software(key_pem),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_signer_emits_csr_and_software_key() {
        let csr = SoftwareP256Signer
            .generate_csr("zerocode")
            .expect("software signer generates a CSR");
        assert!(csr.csr_pem.contains("BEGIN CERTIFICATE REQUEST"));
        match csr.key {
            ClientKey::Software(pem) => {
                let pkcs8_marker = concat!("BEGIN ", "PRIVATE KEY");
                let ec_marker = concat!("BEGIN EC ", "PRIVATE KEY");
                assert!(
                    pem.contains(pkcs8_marker) || pem.contains(ec_marker),
                    "expected a PKCS#8 / EC private key PEM"
                );
            }
            ClientKey::HardwareAlias(_) => panic!("desktop signer must return a software key"),
        }
    }

    #[test]
    fn software_signer_csr_is_signable_by_a_ca() {
        // The seam's output must enroll exactly like the raw helper: the CA signs
        // the CSR and stamps its OWN identity, discarding the requested subject.
        let (ca_crt, ca_key) = crate::testing::gen_ca();
        let csr = SoftwareP256Signer
            .generate_csr("ignored-hint")
            .expect("signer generates a CSR");
        let leaf = crate::sign_csr(&ca_crt, &ca_key, "dev_abc", &csr.csr_pem)
            .expect("CA signs the software CSR");
        assert!(leaf.cert_pem.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn hardware_alias_seam_is_representable() {
        // The mobile seam: a non-exportable key surfaces only as an alias. There
        // is no in-tree producer, but the type must round-trip for out-of-repo
        // hardware signers (Secure Enclave / Android Keystore).
        let csr = ClientCsr {
            csr_pem: "x".to_string(),
            key: ClientKey::HardwareAlias("keystore://zeroclaw/client".to_string()),
        };
        match csr.key {
            ClientKey::HardwareAlias(a) => assert!(a.starts_with("keystore://")),
            ClientKey::Software(_) => panic!("expected a hardware alias"),
        }
    }
}
