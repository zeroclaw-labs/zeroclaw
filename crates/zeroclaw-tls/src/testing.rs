//! Shared certificate / CSR generation fixtures for downstream test code.
//!
//! Enabled by the `testing` feature. The daemon enrollment tests, the relay
//! integration tests, and this crate's own tests all reuse these helpers instead
//! of duplicating rcgen boilerplate. These are test fixtures, not production
//! issuance: production issuance lives in [`crate::certgen`].

/// Generate a self-signed CA with the production CA profile
/// (`CA:TRUE, pathlen:0`, `keyCertSign + cRLSign`). Returns `(cert_pem, key_pem)`.
pub fn gen_ca() -> (String, String) {
    let key = rcgen::KeyPair::generate().expect("generate CA key");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "ZeroClaw Test CA");
    params.distinguished_name = dn;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let cert = params.self_signed(&key).expect("self-sign CA");
    (cert.pem(), key.serialize_pem())
}

/// Generate a `serverAuth` leaf signed by the given CA, with the given SANs.
/// Returns `(cert_pem, key_pem)`.
pub fn gen_server_cert(ca_cert_pem: &str, ca_key_pem: &str, sans: &[String]) -> (String, String) {
    let ca_key = rcgen::KeyPair::from_pem(ca_key_pem).expect("load CA key");
    let ca = rcgen::CertificateParams::from_ca_cert_pem(ca_cert_pem)
        .expect("load CA cert")
        .self_signed(&ca_key)
        .expect("reconstruct CA");
    let key = rcgen::KeyPair::generate().expect("generate server key");
    let mut params = rcgen::CertificateParams::new(sans.to_vec()).expect("server params");
    params.is_ca = rcgen::IsCa::NoCa;
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params
        .signed_by(&key, &ca, &ca_key)
        .expect("sign server cert");
    (cert.pem(), key.serialize_pem())
}

/// Generate a client CSR (ECDSA P-256, like the real device path) requesting
/// subject CN = `device_id`. Returns `(csr_pem, device_key_pem)`; the key is the
/// device-local key that never leaves the device in the real enrollment flow.
pub fn gen_client_csr(device_id: &str) -> (String, String) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("generate P-256");
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("csr params");
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, device_id);
    params.distinguished_name = dn;
    let csr = params.serialize_request(&key).expect("serialize CSR");
    (csr.pem().expect("CSR pem"), key.serialize_pem())
}

/// Generate a client CSR that *requests* an attacker-controlled subject CN plus
/// extra SAN entries, used to prove the issuer ignores CSR-supplied identity
/// (threat A7). Returns `(csr_pem, device_key_pem)`.
pub fn gen_client_csr_injecting(requested_cn: &str, requested_sans: &[String]) -> (String, String) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("generate P-256");
    let mut params = rcgen::CertificateParams::new(requested_sans.to_vec()).expect("csr params");
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, requested_cn);
    params.distinguished_name = dn;
    // Also request a serverAuth EKU to confirm the issuer overrides it to clientAuth.
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let csr = params.serialize_request(&key).expect("serialize CSR");
    (csr.pem().expect("CSR pem"), key.serialize_pem())
}
