//! Client-side crypto helpers for enrollment and relay outer-TLS pinning.
//!
//! This module intentionally stays inside zerocode so the TUI does not link
//! backend `zeroclaw-*` crates. The wire contract is the generated CSR, the SAS
//! string, and the relay certificate fingerprint.

use std::sync::Mutex;

use anyhow::{Context, Result};
use rustls::pki_types::CertificateDer;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub fn cert_sha256_fingerprint(cert_der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hex::encode(hasher.finalize())
}

pub fn enrollment_sas(pairing_code: &str, ca_fingerprint_hex: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"zeroclaw-enroll-sas-v1\0");
    hasher.update(pairing_code.trim().as_bytes());
    hasher.update([0u8]);
    hasher.update(ca_fingerprint_hex.trim().to_lowercase().as_bytes());
    let digest = hex::encode(hasher.finalize());
    let s = digest[..8].to_uppercase();
    format!("{}-{}", &s[..4], &s[4..8])
}

pub fn generate_client_csr(subject_hint: &str) -> Result<(String, Zeroizing<String>)> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .context("generating client key")?;
    let mut params =
        rcgen::CertificateParams::new(Vec::<String>::new()).context("building CSR params")?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, subject_hint);
    params.distinguished_name = dn;

    let csr = params
        .serialize_request(&key)
        .context("serializing certificate signing request")?;
    let csr_pem = csr.pem().context("encoding CSR as PEM")?;
    Ok((csr_pem, Zeroizing::new(key.serialize_pem())))
}

#[derive(Debug)]
pub struct RelayPinVerifier {
    expected: Option<String>,
    tofu: bool,
    observed: Mutex<Option<String>>,
    algs: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl RelayPinVerifier {
    pub fn pinned(expected_sha256_hex: impl Into<String>) -> Self {
        Self {
            expected: Some(expected_sha256_hex.into()),
            tofu: false,
            observed: Mutex::new(None),
            algs: Self::algs(),
        }
    }

    pub fn tofu() -> Self {
        Self {
            expected: None,
            tofu: true,
            observed: Mutex::new(None),
            algs: Self::algs(),
        }
    }

    pub fn observed_pin(&self) -> Option<String> {
        self.observed.lock().expect("pin lock").clone()
    }

    fn algs() -> rustls::crypto::WebPkiSupportedAlgorithms {
        rustls::crypto::ring::default_provider().signature_verification_algorithms
    }
}

impl rustls::client::danger::ServerCertVerifier for RelayPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = cert_sha256_fingerprint(end_entity.as_ref());
        if let Some(want) = &self.expected {
            if fp.eq_ignore_ascii_case(want) {
                return Ok(rustls::client::danger::ServerCertVerified::assertion());
            }
            return Err(rustls::Error::General(format!(
                "relay outer-cert pin mismatch (expected {want}, got {fp})"
            )));
        }
        if self.tofu {
            *self.observed.lock().expect("pin lock") = Some(fp);
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }
        Err(rustls::Error::General(
            "relay pin verifier requires a pin or TOFU".into(),
        ))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algs.supported_schemes()
    }
}

#[cfg(test)]
pub(crate) mod test_pki {
    use std::sync::Arc;

    fn distinguished_name(common_name: &str) -> rcgen::DistinguishedName {
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, common_name);
        dn
    }

    pub(crate) fn gen_ca() -> (String, rcgen::Certificate, rcgen::KeyPair) {
        let key = rcgen::KeyPair::generate().expect("generate CA key");
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.distinguished_name = distinguished_name("ZeroClaw Test CA");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let cert = params.self_signed(&key).expect("self-sign CA");
        (cert.pem(), cert, key)
    }

    pub(crate) fn gen_server_cert(
        ca: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
        sans: &[String],
    ) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("generate server key");
        let mut params = rcgen::CertificateParams::new(sans.to_vec()).expect("server params");
        params.is_ca = rcgen::IsCa::NoCa;
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let cert = params
            .signed_by(&key, ca, ca_key)
            .expect("sign server cert");
        (cert.pem(), key.serialize_pem())
    }

    pub(crate) fn sign_csr(
        ca: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
        device_id: &str,
        csr_pem: &str,
    ) -> String {
        let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).expect("parse CSR");
        let mut params =
            rcgen::CertificateParams::new(Vec::<String>::new()).expect("client params");
        params.distinguished_name = distinguished_name(device_id);
        params.is_ca = rcgen::IsCa::NoCa;
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        csr.params = params;
        csr.signed_by(ca, ca_key).expect("sign CSR").pem()
    }

    pub(crate) fn tls_acceptor(cert_pem: &str, key_pem: &str) -> tokio_rustls::TlsAcceptor {
        let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("parse certs");
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .expect("parse key")
            .expect("private key present");
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("safe protocols")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("server cert/key");
        tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sas_is_deterministic_and_ca_sensitive() {
        let a = enrollment_sas("270391", "aa11bb22");
        assert_eq!(a, enrollment_sas("270391", "AA11BB22"));
        assert_ne!(a, enrollment_sas("270391", "cc33dd44"));
        assert_ne!(a, enrollment_sas("999999", "aa11bb22"));
    }

    #[test]
    fn client_csr_contains_request_and_key() {
        let (csr_pem, key_pem) = generate_client_csr("zerocode").unwrap();
        assert!(csr_pem.contains("BEGIN CERTIFICATE REQUEST"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY") || key_pem.contains("BEGIN EC PRIVATE KEY"));
    }
}
