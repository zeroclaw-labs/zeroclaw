//! Dependency-version regression lock: it tests rustls-webpki, not zeroclaw-tls code.
//!
//! Public issuer fixtures captured on 2026-10-20 from the incident response
//! at https://emudhra.we-have-a-problem.com/. The leaf uses the neutral
//! host.invalid name and was signed offline with the disclosed weak scalar.
//! These fixtures contain certificates only, never private keys. Both issuer paths
//! must fail even when the operating system still supplies the old root.

use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

fn certificates(pem: &[u8]) -> Vec<CertificateDer<'static>> {
    rustls_pemfile::certs(&mut Cursor::new(pem))
        .collect::<Result<Vec<_>, _>>()
        .expect("public certificate fixture parses")
}

fn assert_compromised_chain_is_distrusted(root: &[u8], chain: &[u8], hostname: &'static str) {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(root) {
        roots.add(certificate).expect("public root parses");
    }
    let verifier = WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .expect("server verifier builds");
    let chain = certificates(chain);
    let leaf = certificates(include_bytes!("fixtures/emudhra/host-invalid-leaf.pem"));
    // Freeze the verification time inside the offline test leaf's validity
    // period, so expiration cannot make this regression pass accidentally.
    let now = UnixTime::since_unix_epoch(Duration::from_secs(1_792_584_000));
    let result = verifier.verify_server_cert(
        &leaf[0],
        &chain,
        &ServerName::try_from(hostname).expect("fixture hostname is valid"),
        &[],
        now,
    );
    let error = result.expect_err("compromised intermediate must never authenticate a server");
    assert!(
        format!("{error:?}").contains("CertDistrusted"),
        "the intermediate must be rejected for distrust, not an unrelated validation failure: {error:?}"
    );
}

#[test]
fn accepts_an_unaffected_ca_at_the_same_verification_time() {
    let ca_key = rcgen::KeyPair::generate().expect("generate unaffected CA key");
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    ca_params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    let ca = ca_params.self_signed(&ca_key).expect("self-sign CA");
    let leaf_key = rcgen::KeyPair::generate().expect("generate leaf key");
    let mut leaf_params =
        rcgen::CertificateParams::new(vec!["host.invalid".into()]).expect("leaf params");
    leaf_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params
        .signed_by(&leaf_key, &ca, &ca_key)
        .expect("sign unaffected leaf");
    let mut roots = RootCertStore::empty();
    roots.add(ca.der().clone()).expect("add unaffected CA");
    let verifier = WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .expect("server verifier builds");
    verifier
        .verify_server_cert(
            leaf.der(),
            &[],
            &ServerName::try_from("host.invalid").expect("fixture hostname is valid"),
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(1_792_584_000)),
        )
        .expect("unaffected CA still authenticates a valid server");
}

#[test]
fn rejects_compromised_emudhra_intermediate_under_emsign_root() {
    assert_compromised_chain_is_distrusted(
        include_bytes!("fixtures/emudhra/emsign-ecc-g3-root.pem"),
        include_bytes!("fixtures/emudhra/emsign-issuer-chain.pem"),
        "host.invalid",
    );
}

#[test]
fn rejects_compromised_emudhra_intermediate_under_aaa_cross_sign() {
    assert_compromised_chain_is_distrusted(
        include_bytes!("fixtures/emudhra/aaa-root.pem"),
        include_bytes!("fixtures/emudhra/aaa-issuer-chain.pem"),
        "host.invalid",
    );
}
