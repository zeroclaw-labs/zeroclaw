//! Shared TLS and mutual TLS (mTLS) construction for ZeroClaw.
//!
//! This crate sits below both `zeroclaw-runtime` and `zeroclaw-gateway` so the
//! same rustls server-config / client-certificate-verifier / certificate-pinning
//! logic can be reused without an upward dependency. It is parameterized by the
//! neutral [`ServerConfigParams`] / [`ClientAuthParams`] types rather than any
//! consumer crate's configuration struct, keeping this crate free of upward
//! dependencies on `zeroclaw-config` and friends.

use anyhow::{Context, Result};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

pub mod certgen;
pub use certgen::{
    CaKeyProtection, IssuedLeaf, Pem, ServerMaterials, ensure_server_materials,
    ensure_server_materials_protected, generate_client_csr, issue_client_cert, load_ca_key_pem,
    sign_csr,
};

pub mod csr;
pub use csr::{ClientCsr, ClientKey, CsrSigner, SoftwareP256Signer};

/// Shared certificate / CSR generation helpers for downstream test code. Public
/// under the `testing` feature so the daemon enrollment and relay integration
/// tests reuse one set of fixtures rather than duplicating rcgen boilerplate;
/// also available to this crate's own tests under `cfg(test)`.
#[cfg(any(test, feature = "testing"))]
pub mod testing;

/// Client-certificate verification parameters (transport-neutral).
///
/// Construct this only when client authentication should be enabled; pass it as
/// [`ServerConfigParams::client_auth`]. A `None` client-auth means server-only
/// TLS.
#[derive(Debug, Clone, Default)]
pub struct ClientAuthParams {
    /// Path to the PEM CA certificate(s) used to verify client certificates.
    pub ca_cert_path: String,
    /// Require a client certificate (vs. allow unauthenticated connections).
    pub require_client_cert: bool,
    /// Optional SHA-256 fingerprints to pin. Colons and case are ignored.
    pub pinned_certs: Vec<String>,
    /// Optional path to a revoked-fingerprint list (one SHA-256 hex per line).
    /// When set, a client certificate whose fingerprint appears in the file is
    /// REFUSED at the handshake (A5). The file is re-read when it changes, so a
    /// revoke takes effect on the next connection (the daemon materializes it from
    /// the issued-cert ledger on every revoke). Empty disables the check.
    pub crl_path: String,
}

/// Server TLS parameters (transport-neutral).
#[derive(Debug, Clone)]
pub struct ServerConfigParams {
    /// Path to the PEM server certificate chain.
    pub cert_path: String,
    /// Path to the PEM server private key.
    pub key_path: String,
    /// `Some` enables client-certificate verification (mTLS); `None` is
    /// server-only TLS.
    pub client_auth: Option<ClientAuthParams>,
}

/// Build a [`TlsAcceptor`] from the given server parameters.
pub fn build_tls_acceptor(params: &ServerConfigParams) -> Result<TlsAcceptor> {
    let server_config = build_server_config(params)?;
    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Build a [`rustls::ServerConfig`] from the given server parameters.
pub fn build_server_config(params: &ServerConfigParams) -> Result<rustls::ServerConfig> {
    let certs = load_certs(&params.cert_path).with_context(|| {
        format!(
            "failed to load server certificate from {}",
            params.cert_path
        )
    })?;
    let key = load_private_key(&params.key_path)
        .with_context(|| format!("failed to load private key from {}", params.key_path))?;

    let builder = rustls::ServerConfig::builder();

    let server_config = if let Some(client_auth) = &params.client_auth {
        let verifier = build_client_verifier(client_auth)
            .context("failed to build client certificate verifier")?;
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .context("invalid server certificate or key")?
    } else {
        builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("invalid server certificate or key")?
    };

    Ok(server_config)
}

/// Build a [`TlsAcceptor`] for a remote, mutually-authenticated transport plane.
///
/// This is the secure-by-construction entrypoint for the daemon's remote WSS
/// plane: the returned acceptor is **TLS 1.3 only** and **always** requires and
/// verifies a client certificate against `ca_cert_path` (optionally pinned to
/// `pinned_certs`). There is deliberately **no** no-client-auth / server-only
/// code path on this function, so the remote plane cannot be weakened by
/// configuration (threat model A11). `ca_cert_path` is mandatory.
pub fn build_mtls_acceptor(
    cert_path: &str,
    key_path: &str,
    ca_cert_path: &str,
    pinned_certs: &[String],
    crl_path: &str,
) -> Result<TlsAcceptor> {
    let server_config =
        build_mtls_server_config(cert_path, key_path, ca_cert_path, pinned_certs, crl_path)?;
    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Build a TLS 1.3-only [`rustls::ServerConfig`] that always requires and
/// verifies a client certificate. See [`build_mtls_acceptor`]; this is the
/// inner config builder. There is no no-client-auth branch here by design.
/// `crl_path`, when non-empty, refuses a revoked client certificate (A5).
pub fn build_mtls_server_config(
    cert_path: &str,
    key_path: &str,
    ca_cert_path: &str,
    pinned_certs: &[String],
    crl_path: &str,
) -> Result<rustls::ServerConfig> {
    let certs = load_certs(cert_path)
        .with_context(|| format!("failed to load server certificate from {cert_path}"))?;
    let key = load_private_key(key_path)
        .with_context(|| format!("failed to load private key from {key_path}"))?;

    // Mandatory client-certificate verification: require_client_cert is forced
    // true so this builder can never produce an unauthenticated acceptor.
    let verifier = build_client_verifier(&ClientAuthParams {
        ca_cert_path: ca_cert_path.to_string(),
        require_client_cert: true,
        pinned_certs: pinned_certs.to_vec(),
        crl_path: crl_path.to_string(),
    })
    .context("failed to build client certificate verifier")?;

    // Pin the protocol to TLS 1.3 only (no TLS 1.2 downgrade) for the remote plane.
    let server_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .context("invalid server certificate or key")?;

    Ok(server_config)
}

/// Build a client certificate verifier from the client-auth parameters.
pub fn build_client_verifier(params: &ClientAuthParams) -> Result<Arc<dyn ClientCertVerifier>> {
    let ca_certs = load_certs(&params.ca_cert_path)
        .with_context(|| format!("failed to load CA certificate from {}", params.ca_cert_path))?;

    let mut root_store = RootCertStore::empty();
    for cert in &ca_certs {
        root_store
            .add(cert.clone())
            .context("failed to add CA certificate to root store")?;
    }

    let base_verifier = if params.require_client_cert {
        WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .context("failed to build WebPKI client verifier")?
    } else {
        WebPkiClientVerifier::builder(Arc::new(root_store))
            .allow_unauthenticated()
            .build()
            .context("failed to build WebPKI client verifier (optional auth)")?
    };

    // Layer the checks outward: WebPKI -> pin -> revocation. A revoked cert is
    // refused even if it passes the CA chain and the pin set.
    let mut verifier = base_verifier;
    if !params.pinned_certs.is_empty() {
        let normalized: Vec<String> = params
            .pinned_certs
            .iter()
            .map(|fp| fp.replace(':', "").to_lowercase())
            .collect();
        verifier = Arc::new(PinnedCertVerifier {
            inner: verifier,
            pinned_fingerprints: normalized,
        });
    }
    if !params.crl_path.trim().is_empty() {
        verifier = Arc::new(
            RevocationCheckVerifier::new(verifier, params.crl_path.trim().into())
                .context("failed to load client certificate revocation list")?,
        );
    }
    Ok(verifier)
}

/// Read a revoked-fingerprint file (one SHA-256 hex per line; blank lines and
/// `#` comments ignored) into a normalized set. Missing/unreadable files are
/// errors: a configured revocation boundary must fail closed, not silently become
/// an empty revocation set.
pub fn load_revoked_fingerprints(
    path: &std::path::Path,
) -> Result<std::collections::HashSet<String>> {
    let mut set = std::collections::HashSet::new();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read revoked fingerprint list {}", path.display()))?;
    for line in text.lines() {
        let fp = line.trim();
        if fp.is_empty() || fp.starts_with('#') {
            continue;
        }
        set.insert(fp.replace(':', "").to_lowercase());
    }
    Ok(set)
}

/// Compute the SHA-256 fingerprint of a DER-encoded certificate.
pub fn cert_sha256_fingerprint(cert_der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    let hash = hasher.finalize();
    hex::encode(hash)
}

/// Read a relay node-id from an OUTER client certificate's subject Common Name.
///
/// Used only by the optional outer-mTLS relay variant: when an operator issues
/// outer client certs whose CN is the target node-id, the relay binds which node
/// a client may reach to its certificate (additive admission), falling back to the
/// `Connect` frame's node-id when the cert carries none. Returns `None` for an
/// unparseable cert or an empty CN. This never touches the inner mTLS.
pub fn client_cert_node_id(cert_der: &[u8]) -> Option<String> {
    use x509_parser::prelude::*;
    let (_, cert) = X509Certificate::from_der(cert_der).ok()?;
    cert.subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The enrollment short-auth-string binding a one-time pairing code to the daemon
/// CA fingerprint (no blind TOFU at bootstrap; threats A1/A7).
///
/// The daemon prints this beside the pairing code. A certless client recomputes
/// it from the code the operator typed plus the CA fingerprint it received over
/// the (server-authenticated, possibly MITM'd) enrollment channel, and the
/// operator compares the two out of band. A MITM that substitutes its own CA
/// yields a different fingerprint and therefore a mismatching SAS, so it cannot
/// impersonate the daemon CA during the very first exchange. Same inputs on both
/// ends must produce the same string, so this lives in the shared crate.
///
/// Returned as two groups of hex for easy visual comparison, e.g. `A1B2-C3D4`.
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

/// A client certificate verifier that delegates to a base verifier and then
/// checks that the presented certificate matches one of the pinned SHA-256
/// fingerprints.
#[derive(Debug)]
struct PinnedCertVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    pinned_fingerprints: Vec<String>,
}

impl ClientCertVerifier for PinnedCertVerifier {
    fn offer_client_auth(&self) -> bool {
        self.inner.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.inner.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        // First, run the standard WebPKI verification.
        self.inner
            .verify_client_cert(end_entity, intermediates, now)?;

        // Then check the fingerprint against the pinned set.
        let fingerprint = cert_sha256_fingerprint(end_entity.as_ref());
        if self.pinned_fingerprints.contains(&fingerprint) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "client certificate fingerprint {fingerprint} is not in the pinned set"
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A client-certificate verifier that delegates to a base verifier and then
/// REFUSES a certificate whose SHA-256 fingerprint is in a revoked-fingerprint
/// file (threat A5: a stolen-but-unexpired cert is rejected at the handshake).
///
/// The file is the daemon's materialized revocation list (the issued-cert ledger
/// rewrites it on every revoke). It is intentionally re-read on every handshake:
/// the file is tiny, and this avoids stale accepts on filesystems with coarse or
/// preserved mtimes.
#[derive(Debug)]
struct RevocationCheckVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    crl_path: std::path::PathBuf,
}

impl RevocationCheckVerifier {
    fn new(inner: Arc<dyn ClientCertVerifier>, crl_path: std::path::PathBuf) -> Result<Self> {
        let _ = load_revoked_fingerprints(&crl_path)?;
        Ok(Self { inner, crl_path })
    }

    /// True if `fingerprint` (normalized) is currently revoked.
    fn is_revoked(&self, fingerprint: &str) -> std::result::Result<bool, rustls::Error> {
        let revoked = load_revoked_fingerprints(&self.crl_path)
            .map_err(|e| revocation_error(&self.crl_path, e))?;
        Ok(revoked.contains(fingerprint))
    }
}

fn revocation_error(path: &std::path::Path, error: impl std::fmt::Display) -> rustls::Error {
    rustls::Error::General(format!(
        "client certificate revocation list {} is unavailable: {error}",
        path.display()
    ))
}

impl ClientCertVerifier for RevocationCheckVerifier {
    fn offer_client_auth(&self) -> bool {
        self.inner.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.inner.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        self.inner
            .verify_client_cert(end_entity, intermediates, now)?;
        let fingerprint = cert_sha256_fingerprint(end_entity.as_ref());
        if self.is_revoked(&fingerprint)? {
            return Err(rustls::Error::General(
                "client certificate has been revoked".to_string(),
            ));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// A rustls SERVER-certificate verifier that PINS the relay's OUTER leaf cert by
/// its SHA-256 fingerprint instead of chaining to a CA (threat A2: the outer TLS
/// is a metadata boundary, not the RPC boundary; the inner mTLS is the real one).
///
/// Two modes:
/// - [`RelayPinVerifier::pinned`]: the presented leaf MUST match the expected
///   fingerprint, else the connection is refused. The handshake signature is still
///   verified against the leaf's key, so a pin is a real cryptographic bind.
/// - [`RelayPinVerifier::tofu`]: trust-on-first-use - accept the first leaf seen
///   and RECORD its fingerprint (read back with [`observed_pin`] and persist it),
///   so the next connection is pinned. Opt-in only; never a silent default.
///
/// The server NAME is intentionally not checked: pinning the exact leaf already
/// fixes the identity, which suits a self-hosted relay reached by IP.
///
/// [`observed_pin`]: RelayPinVerifier::observed_pin
#[derive(Debug)]
pub struct RelayPinVerifier {
    /// Expected leaf SHA-256 (hex). `None` in TOFU mode.
    expected: Option<String>,
    /// Accept + record the first leaf when no pin is set.
    tofu: bool,
    /// The leaf fingerprint observed during the handshake (TOFU persistence).
    observed: std::sync::Mutex<Option<String>>,
    algs: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl RelayPinVerifier {
    /// Require the relay's outer leaf to match `expected_sha256_hex`.
    pub fn pinned(expected_sha256_hex: impl Into<String>) -> Self {
        Self {
            expected: Some(expected_sha256_hex.into()),
            tofu: false,
            observed: std::sync::Mutex::new(None),
            algs: Self::algs(),
        }
    }

    /// Trust-on-first-use: accept the first leaf and record its fingerprint.
    pub fn tofu() -> Self {
        Self {
            expected: None,
            tofu: true,
            observed: std::sync::Mutex::new(None),
            algs: Self::algs(),
        }
    }

    /// The leaf fingerprint observed during a TOFU handshake, if any. Persist it so
    /// later connections pin instead of re-trusting.
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

/// Load PEM-encoded certificates from a file.
pub fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open certificate file: {path}"))?;
    let mut reader = std::io::BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse PEM certificates from {path}"))?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in {path}");
    }
    Ok(certs)
}

/// Load a PEM-encoded private key from a file.
pub fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open private key file: {path}"))?;
    let mut reader = std::io::BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("failed to parse private key from {path}"))?
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"path": path})),
                "TLS private key file contains no key"
            );
            anyhow::Error::msg(format!("no private key found in {path}"))
        })?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure the rustls `CryptoProvider` is installed (idempotent).
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn client_cert_node_id_reads_cn() {
        // The outer-mTLS variant binds the target node-id to the client cert CN.
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "nodeid-abc123");
        params.distinguished_name = dn;
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        assert_eq!(
            client_cert_node_id(cert.der().as_ref()).as_deref(),
            Some("nodeid-abc123")
        );
        // Garbage DER -> None, never a panic (the relay then uses the Connect frame).
        assert!(client_cert_node_id(b"not a cert").is_none());
    }

    #[test]
    fn load_revoked_fingerprints_normalizes_and_skips_comments() {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            f.path(),
            "# a comment\nAA:BB:CC\n\n  deadBEEF  \n# another\n",
        )
        .unwrap();
        let set = load_revoked_fingerprints(f.path()).unwrap();
        assert!(set.contains("aabbcc"), "colons + case normalized");
        assert!(set.contains("deadbeef"), "trimmed + lowercased");
        assert_eq!(set.len(), 2, "comments + blanks skipped");
        let err = load_revoked_fingerprints(std::path::Path::new("/no/such/crl"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("revoked fingerprint list"), "got: {err}");
    }

    #[test]
    fn revocation_verifier_refuses_a_revoked_client_cert() {
        ensure_crypto_provider();
        // A real CA + a clientAuth leaf it issued (so WebPKI validation passes and
        // the ONLY thing that can reject is revocation).
        let (ca_crt, ca_key) = testing::gen_ca();
        let (csr, _key) = testing::gen_client_csr("dev_revoke");
        let leaf = sign_csr(&ca_crt, &ca_key, "dev_revoke", &csr).unwrap();
        let leaf_der = rustls_pemfile::certs(&mut leaf.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        let fp = cert_sha256_fingerprint(leaf_der.as_ref());

        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &ca_crt).unwrap();
        let now = rustls::pki_types::UnixTime::now();

        // CRL lists this fingerprint -> the handshake is refused (A5).
        let revoked_crl = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(revoked_crl.path(), format!("{fp}\n")).unwrap();
        let revoking = build_client_verifier(&ClientAuthParams {
            ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
            require_client_cert: true,
            pinned_certs: vec![],
            crl_path: revoked_crl.path().to_str().unwrap().to_string(),
        })
        .unwrap();
        assert!(
            revoking.verify_client_cert(&leaf_der, &[], now).is_err(),
            "a revoked client cert must be refused"
        );

        // An empty CRL -> the same cert is accepted (revocation is the only gate
        // that changed). A separate verifier avoids any mtime-cache timing.
        let empty_crl = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(empty_crl.path(), "").unwrap();
        let allowing = build_client_verifier(&ClientAuthParams {
            ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
            require_client_cert: true,
            pinned_certs: vec![],
            crl_path: empty_crl.path().to_str().unwrap().to_string(),
        })
        .unwrap();
        assert!(
            allowing.verify_client_cert(&leaf_der, &[], now).is_ok(),
            "an un-revoked client cert passes"
        );
    }

    #[test]
    fn revocation_verifier_fails_closed_when_crl_file_disappears() {
        ensure_crypto_provider();
        let (ca_crt, ca_key) = testing::gen_ca();
        let (csr, _key) = testing::gen_client_csr("dev_revoke");
        let leaf = sign_csr(&ca_crt, &ca_key, "dev_revoke", &csr).unwrap();
        let leaf_der = rustls_pemfile::certs(&mut leaf.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();

        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &ca_crt).unwrap();
        let crl = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(crl.path(), "").unwrap();
        let verifier = build_client_verifier(&ClientAuthParams {
            ca_cert_path: ca_file.path().to_string_lossy().into_owned(),
            require_client_cert: true,
            pinned_certs: vec![],
            crl_path: crl.path().to_string_lossy().into_owned(),
        })
        .unwrap();
        std::fs::remove_file(crl.path()).unwrap();

        let err = verifier
            .verify_client_cert(&leaf_der, &[], rustls::pki_types::UnixTime::now())
            .unwrap_err()
            .to_string();
        assert!(err.contains("revocation list"), "got: {err}");
    }

    #[test]
    fn revocation_verifier_reloads_when_contents_change_but_mtime_does_not() {
        ensure_crypto_provider();
        let (ca_crt, ca_key) = testing::gen_ca();
        let (csr, _key) = testing::gen_client_csr("dev_revoke");
        let leaf = sign_csr(&ca_crt, &ca_key, "dev_revoke", &csr).unwrap();
        let leaf_der = rustls_pemfile::certs(&mut leaf.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        let fp = cert_sha256_fingerprint(leaf_der.as_ref());

        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), &ca_crt).unwrap();
        let crl = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(crl.path(), "").unwrap();
        let fixed_mtime = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(crl.path()).unwrap(),
        );
        let verifier = build_client_verifier(&ClientAuthParams {
            ca_cert_path: ca_file.path().to_string_lossy().into_owned(),
            require_client_cert: true,
            pinned_certs: vec![],
            crl_path: crl.path().to_string_lossy().into_owned(),
        })
        .unwrap();
        let now = rustls::pki_types::UnixTime::now();
        assert!(verifier.verify_client_cert(&leaf_der, &[], now).is_ok());

        std::fs::write(crl.path(), format!("{fp}\n")).unwrap();
        filetime::set_file_mtime(crl.path(), fixed_mtime).unwrap();
        let err = verifier
            .verify_client_cert(&leaf_der, &[], now)
            .unwrap_err()
            .to_string();
        assert!(err.contains("revoked"), "got: {err}");
    }

    #[test]
    fn verifier_supports_dual_trust_anchors_ca_rotation_seam() {
        // CA-rotation seam (the one accepted, design-sanctioned deferral): full CA
        // rotation orchestration is out of scope for v1, but the verifier's
        // RootCertStore already holds MULTIPLE trust anchors, so a future rotation
        // could trust both the OLD and NEW CA during an overlap window and clients
        // issued by EITHER verify. This proves the seam is representable; nothing
        // here orchestrates a rotation.
        ensure_crypto_provider();
        let (ca_old, _k_old) = testing::gen_ca();
        let (ca_new, k_new) = testing::gen_ca();

        // A client cert issued by the NEW anchor.
        let (csr, _key) = testing::gen_client_csr("dev_rotated");
        let leaf = sign_csr(&ca_new, &k_new, "dev_rotated", &csr).unwrap();
        let leaf_der = rustls_pemfile::certs(&mut leaf.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();

        // One CA file holding BOTH anchors (old + new) - the overlap-window trust.
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), format!("{ca_old}\n{ca_new}")).unwrap();
        let verifier = build_client_verifier(&ClientAuthParams {
            ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
            require_client_cert: true,
            pinned_certs: vec![],
            crl_path: String::new(),
        })
        .unwrap();

        let now = rustls::pki_types::UnixTime::now();
        assert!(
            verifier.verify_client_cert(&leaf_der, &[], now).is_ok(),
            "a cert from the second (new) trust anchor must verify - the rotation seam"
        );

        // A cert from a THIRD, untrusted CA is still rejected (not accept-all).
        let (ca_rogue, k_rogue) = testing::gen_ca();
        let (csr2, _k2) = testing::gen_client_csr("dev_rogue");
        let rogue = sign_csr(&ca_rogue, &k_rogue, "dev_rogue", &csr2).unwrap();
        let rogue_der = rustls_pemfile::certs(&mut rogue.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        assert!(
            verifier.verify_client_cert(&rogue_der, &[], now).is_err(),
            "a cert from an untrusted CA must still be rejected"
        );
    }

    #[test]
    fn relay_pin_verifier_matches_rejects_and_tofu_records() {
        use rustls::client::danger::ServerCertVerifier as _;
        ensure_crypto_provider();

        let (ca_crt, ca_key) = testing::gen_ca();
        let (server_pem, _key) = testing::gen_server_cert(&ca_crt, &ca_key, &["localhost".into()]);
        let der = rustls_pemfile::certs(&mut server_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        let fp = cert_sha256_fingerprint(der.as_ref());
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let now = rustls::pki_types::UnixTime::now();

        // Pinned to the right leaf -> accepted.
        let pinned = RelayPinVerifier::pinned(fp.clone());
        assert!(
            pinned
                .verify_server_cert(&der, &[], &name, &[], now)
                .is_ok()
        );

        // Pinned to a different leaf -> rejected (no chaining to a CA).
        let wrong = RelayPinVerifier::pinned("00".repeat(32));
        assert!(
            wrong
                .verify_server_cert(&der, &[], &name, &[], now)
                .is_err()
        );

        // TOFU -> accepts and records the observed fingerprint for persistence.
        let tofu = RelayPinVerifier::tofu();
        assert!(tofu.observed_pin().is_none());
        assert!(tofu.verify_server_cert(&der, &[], &name, &[], now).is_ok());
        assert_eq!(tofu.observed_pin().as_deref(), Some(fp.as_str()));
    }

    #[test]
    fn enrollment_sas_is_deterministic_and_ca_sensitive() {
        let a = enrollment_sas("270391", "aa11bb22");
        // Same inputs -> same SAS (daemon and client must agree).
        assert_eq!(a, enrollment_sas("270391", "AA11BB22"));
        // A different CA fingerprint (a MITM CA) -> a different SAS.
        assert_ne!(a, enrollment_sas("270391", "cc33dd44"));
        // A different code -> a different SAS.
        assert_ne!(a, enrollment_sas("999999", "aa11bb22"));
        // Shape: GROUP-GROUP, 4 uppercase hex each.
        assert_eq!(a.len(), 9);
        assert_eq!(&a[4..5], "-");
        assert!(
            a.chars()
                .filter(|c| *c != '-')
                .all(|c| c.is_ascii_hexdigit())
        );
    }

    /// Generate a self-signed CA cert + key pair.
    /// Returns (cert_pem, key_pem, key_pair) so the key can be reused for signing.
    fn test_ca() -> (String, String, rcgen::KeyPair) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        (ca_cert.pem(), ca_key.serialize_pem(), ca_key)
    }

    /// Generate a server certificate signed by the given CA.
    fn test_server_cert(ca_cert_pem: &str, ca_key: &rcgen::KeyPair) -> (String, String) {
        // Re-parse the CA cert for signing.
        let ca_key_clone = rcgen::KeyPair::from_pem(&ca_key.serialize_pem()).unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key_clone).unwrap();

        let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        server_params.is_ca = rcgen::IsCa::NoCa;
        let server_key = rcgen::KeyPair::generate().unwrap();
        let server_cert = server_params
            .signed_by(&server_key, &ca, &ca_key_clone)
            .unwrap();
        let _ = ca_cert_pem;
        (server_cert.pem(), server_key.serialize_pem())
    }

    fn write_temp_file(content: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    fn server_params(
        cert_path: &str,
        key_path: &str,
        client_auth: Option<ClientAuthParams>,
    ) -> ServerConfigParams {
        ServerConfigParams {
            cert_path: cert_path.to_string(),
            key_path: key_path.to_string(),
            client_auth,
        }
    }

    #[test]
    fn test_load_valid_cert_and_key() {
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);

        let certs = load_certs(cert_file.path().to_str().unwrap()).unwrap();
        assert!(!certs.is_empty());

        let _key = load_private_key(key_file.path().to_str().unwrap()).unwrap();
    }

    #[test]
    fn test_invalid_cert_path_produces_clear_error() {
        let err = load_certs("/nonexistent/path/cert.pem").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cannot open certificate file"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_invalid_key_path_produces_clear_error() {
        let err = load_private_key("/nonexistent/path/key.pem").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cannot open private key file"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_build_server_config_no_client_auth() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);

        // Should build successfully without client auth.
        let _server_config = build_server_config(&server_params(
            cert_file.path().to_str().unwrap(),
            key_file.path().to_str().unwrap(),
            None,
        ))
        .unwrap();
    }

    #[test]
    fn test_build_server_config_with_client_auth() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);
        let ca_file = write_temp_file(&ca_cert_pem);

        // Should build successfully with mandatory client auth.
        let _server_config = build_server_config(&server_params(
            cert_file.path().to_str().unwrap(),
            key_file.path().to_str().unwrap(),
            Some(ClientAuthParams {
                ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
                require_client_cert: true,
                pinned_certs: vec![],
                crl_path: String::new(),
            }),
        ))
        .unwrap();
    }

    #[test]
    fn test_build_server_config_client_auth_optional() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);
        let ca_file = write_temp_file(&ca_cert_pem);

        // Should build successfully with optional client auth.
        let _server_config = build_server_config(&server_params(
            cert_file.path().to_str().unwrap(),
            key_file.path().to_str().unwrap(),
            Some(ClientAuthParams {
                ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
                require_client_cert: false,
                pinned_certs: vec![],
                crl_path: String::new(),
            }),
        ))
        .unwrap();
    }

    #[test]
    fn test_cert_fingerprint_matching() {
        let (ca_cert_pem, _ca_key_pem, _ca_key) = test_ca();
        let ca_file = write_temp_file(&ca_cert_pem);
        let certs = load_certs(ca_file.path().to_str().unwrap()).unwrap();
        let fingerprint = cert_sha256_fingerprint(certs[0].as_ref());

        // Fingerprint should be a 64-char hex string (SHA-256).
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));

        // Same cert should produce the same fingerprint.
        let fingerprint2 = cert_sha256_fingerprint(certs[0].as_ref());
        assert_eq!(fingerprint, fingerprint2);
    }

    #[test]
    fn test_fingerprint_differs_for_different_certs() {
        let (ca_cert_pem1, _, _) = test_ca();
        let (ca_cert_pem2, _, _) = test_ca();
        let f1 = write_temp_file(&ca_cert_pem1);
        let f2 = write_temp_file(&ca_cert_pem2);
        let certs1 = load_certs(f1.path().to_str().unwrap()).unwrap();
        let certs2 = load_certs(f2.path().to_str().unwrap()).unwrap();
        let fp1 = cert_sha256_fingerprint(certs1[0].as_ref());
        let fp2 = cert_sha256_fingerprint(certs2[0].as_ref());
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_build_server_config_with_pinning() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);
        let ca_file = write_temp_file(&ca_cert_pem);

        // Should build successfully - pinning is checked at connection time, not config time.
        let _server_config = build_server_config(&server_params(
            cert_file.path().to_str().unwrap(),
            key_file.path().to_str().unwrap(),
            Some(ClientAuthParams {
                ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
                require_client_cert: true,
                pinned_certs: vec!["aabbccdd".to_string()],
                crl_path: String::new(),
            }),
        ))
        .unwrap();
    }

    #[test]
    fn test_empty_cert_file_produces_error() {
        let empty_file = write_temp_file("");
        let err = load_certs(empty_file.path().to_str().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no certificates found"),
            "unexpected error: {msg}"
        );
    }

    // ---- mandatory-mTLS end-to-end handshake matrix (build_mtls_server_config) ----
    //
    // These drive a real in-memory rustls handshake between a server built from
    // build_mtls_server_config() and a client, asserting the security invariants:
    // mandatory client auth, CA chaining, pinning, and TLS 1.3 negotiation.

    use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

    fn gen_ca() -> (rcgen::Certificate, rcgen::KeyPair) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
        p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = p.self_signed(&key).unwrap();
        (cert, key)
    }

    fn gen_leaf(
        ca: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
        name: &str,
        client: bool,
    ) -> (rcgen::Certificate, rcgen::KeyPair) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(vec![name.into()]).unwrap();
        p.is_ca = rcgen::IsCa::NoCa;
        p.extended_key_usages = vec![if client {
            rcgen::ExtendedKeyUsagePurpose::ClientAuth
        } else {
            rcgen::ExtendedKeyUsagePurpose::ServerAuth
        }];
        let cert = p.signed_by(&key, ca, ca_key).unwrap();
        (cert, key)
    }

    fn key_der(key: &rcgen::KeyPair) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(key.serialize_der().into())
    }

    fn client_config(
        trusted_ca: &CertificateDer<'static>,
        client_identity: Option<(CertificateDer<'static>, PrivateKeyDer<'static>)>,
    ) -> rustls::ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(trusted_ca.clone()).unwrap();
        let builder =
            rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_root_certificates(roots);
        match client_identity {
            Some((cert, key)) => builder.with_client_auth_cert(vec![cert], key).unwrap(),
            None => builder.with_no_client_auth(),
        }
    }

    /// Drive an in-memory rustls handshake; returns the negotiated protocol
    /// version on success, or the rustls error that aborted it.
    fn do_handshake(
        server: rustls::ServerConfig,
        client: rustls::ClientConfig,
    ) -> std::result::Result<rustls::ProtocolVersion, rustls::Error> {
        let mut srv = rustls::ServerConnection::new(Arc::new(server)).unwrap();
        let name = ServerName::try_from("localhost".to_string()).unwrap();
        let mut cli = rustls::ClientConnection::new(Arc::new(client), name).unwrap();

        for _ in 0..40 {
            let mut to_srv = Vec::new();
            while cli.wants_write() {
                cli.write_tls(&mut to_srv).unwrap();
            }
            if !to_srv.is_empty() {
                let mut cur = &to_srv[..];
                while !cur.is_empty() {
                    srv.read_tls(&mut cur).unwrap();
                }
                srv.process_new_packets()?;
            }

            let mut to_cli = Vec::new();
            while srv.wants_write() {
                srv.write_tls(&mut to_cli).unwrap();
            }
            if !to_cli.is_empty() {
                let mut cur = &to_cli[..];
                while !cur.is_empty() {
                    cli.read_tls(&mut cur).unwrap();
                }
                cli.process_new_packets()?;
            }

            if !cli.is_handshaking() && !srv.is_handshaking() {
                return Ok(srv.protocol_version().expect("negotiated version"));
            }
        }
        Err(rustls::Error::General("handshake did not complete".into()))
    }

    /// Build a server via the PUBLIC build_mtls_server_config() API (file paths),
    /// keeping the temp files alive for the duration of the test.
    fn mtls_server(
        ca: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
        pinned: &[String],
    ) -> (
        rustls::ServerConfig,
        tempfile::NamedTempFile,
        tempfile::NamedTempFile,
        tempfile::NamedTempFile,
    ) {
        let (server_cert, server_key) = gen_leaf(ca, ca_key, "localhost", false);
        let cert_f = write_temp_file(&server_cert.pem());
        let key_f = write_temp_file(&server_key.serialize_pem());
        let ca_f = write_temp_file(&ca.pem());
        let cfg = build_mtls_server_config(
            cert_f.path().to_str().unwrap(),
            key_f.path().to_str().unwrap(),
            ca_f.path().to_str().unwrap(),
            pinned,
            "",
        )
        .unwrap();
        (cfg, cert_f, key_f, ca_f)
    }

    #[test]
    fn mtls_valid_client_cert_accepted_negotiates_tls13() {
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        let (client_cert, client_key) = gen_leaf(&ca, &ca_key, "client", true);
        let cli = client_config(
            ca.der(),
            Some((client_cert.der().clone(), key_der(&client_key))),
        );
        let version = do_handshake(srv_cfg, cli).expect("handshake should succeed");
        assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    }

    /// Assert a handshake was ACTIVELY rejected for an expected reason. Critically
    /// this fails if the handshake merely stalled (the do_handshake non-completion
    /// sentinel), so a regression that broke client-auth in a way that aborts the
    /// handshake early cannot pass as a "rejection" (review finding: stall masking).
    fn expect_rejected(
        result: std::result::Result<rustls::ProtocolVersion, rustls::Error>,
        any_of: &[&str],
    ) {
        let err = result.expect_err("handshake must be rejected, but it succeeded");
        let dbg = format!("{err:?}");
        assert!(
            !dbg.contains("handshake did not complete"),
            "handshake STALLED rather than being actively rejected (this would mask a \
             dropped-client-auth regression): {dbg}"
        );
        assert!(
            any_of.iter().any(|s| dbg.contains(s)),
            "rejection reason {dbg} did not match any expected cause in {any_of:?}"
        );
    }

    /// Generate a client leaf with explicit validity bounds (for expiry tests).
    fn gen_client_leaf_validity(
        ca: &rcgen::Certificate,
        ca_key: &rcgen::KeyPair,
        not_before: time::OffsetDateTime,
        not_after: time::OffsetDateTime,
    ) -> (rcgen::Certificate, rcgen::KeyPair) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = rcgen::CertificateParams::new(vec!["client".into()]).unwrap();
        p.is_ca = rcgen::IsCa::NoCa;
        p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        p.not_before = not_before;
        p.not_after = not_after;
        let cert = p.signed_by(&key, ca, ca_key).unwrap();
        (cert, key)
    }

    /// A client config restricted to a specific protocol version, presenting a cert.
    fn client_config_versions(
        versions: &[&'static rustls::SupportedProtocolVersion],
        trusted_ca: &CertificateDer<'static>,
        cert: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
    ) -> rustls::ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(trusted_ca.clone()).unwrap();
        rustls::ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![cert], key)
            .unwrap()
    }

    #[test]
    fn mtls_missing_client_cert_rejected() {
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        // Client presents NO certificate; the mandatory verifier must reject it,
        // and it must be the cert requirement (not an unrelated abort) that does so.
        let cli = client_config(ca.der(), None);
        expect_rejected(
            do_handshake(srv_cfg, cli),
            &[
                "NoCertificatesPresented",
                "CertificateRequired",
                "certificate required",
            ],
        );
    }

    #[test]
    fn mtls_client_cert_from_wrong_ca_rejected() {
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (other_ca, other_ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        // Client trusts the server CA but presents a cert signed by a DIFFERENT CA.
        let (rogue_cert, rogue_key) = gen_leaf(&other_ca, &other_ca_key, "client", true);
        let cli = client_config(
            ca.der(),
            Some((rogue_cert.der().clone(), key_der(&rogue_key))),
        );
        expect_rejected(
            do_handshake(srv_cfg, cli),
            &["UnknownIssuer", "InvalidCertificate"],
        );
    }

    #[test]
    fn mtls_pinned_mismatch_rejected_but_pinned_match_accepted() {
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (client_cert, client_key) = gen_leaf(&ca, &ca_key, "client", true);
        let client_fp = cert_sha256_fingerprint(client_cert.der().as_ref());

        // Pinned to a bogus fingerprint: a valid CA-signed client is still rejected,
        // and specifically by the PINNING layer (its unique error string), not webpki.
        let (srv_bad, _c1, _k1, _a1) = mtls_server(&ca, &ca_key, &["deadbeef".to_string()]);
        let cli_bad = client_config(
            ca.der(),
            Some((client_cert.der().clone(), key_der(&client_key))),
        );
        expect_rejected(do_handshake(srv_bad, cli_bad), &["not in the pinned set"]);

        // Pinned to the real client fingerprint: accepted.
        let (srv_ok, _c2, _k2, _a2) = mtls_server(&ca, &ca_key, &[client_fp]);
        let cli_ok = client_config(
            ca.der(),
            Some((client_cert.der().clone(), key_der(&client_key))),
        );
        do_handshake(srv_ok, cli_ok).expect("pinned client cert must be accepted");
    }

    #[test]
    fn mtls_tls12_client_rejected_no_downgrade() {
        // Threat A11: the remote plane is TLS 1.3 only; a TLS-1.2 client (even with a
        // valid cert) must be refused. tls12 is a compiled-in rustls feature, so this
        // guards the protocol-version pin against a silent widening regression.
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        let (client_cert, client_key) = gen_leaf(&ca, &ca_key, "client", true);
        let cli = client_config_versions(
            &[&rustls::version::TLS12],
            ca.der(),
            client_cert.der().clone(),
            key_der(&client_key),
        );
        expect_rejected(
            do_handshake(srv_cfg, cli),
            &[
                "PeerIncompatible",
                "NoSupportedVersions",
                "protocol version",
            ],
        );
    }

    #[test]
    fn mtls_wrong_eku_server_cert_as_client_rejected() {
        // Threat A7: a serverAuth-only leaf presented as a client cert must be rejected.
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        let (server_eku_cert, key) = gen_leaf(&ca, &ca_key, "client", false); // serverAuth EKU
        let cli = client_config(
            ca.der(),
            Some((server_eku_cert.der().clone(), key_der(&key))),
        );
        expect_rejected(
            do_handshake(srv_cfg, cli),
            &["InvalidPurpose", "InvalidCertificate", "purpose"],
        );
    }

    #[test]
    fn mtls_expired_client_cert_rejected() {
        // Threat A5: an expired client cert must be rejected (validity-window enforced).
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        let now = time::OffsetDateTime::now_utc();
        let (expired, key) = gen_client_leaf_validity(
            &ca,
            &ca_key,
            now - time::Duration::days(30),
            now - time::Duration::days(1),
        );
        let cli = client_config(ca.der(), Some((expired.der().clone(), key_der(&key))));
        expect_rejected(
            do_handshake(srv_cfg, cli),
            &["Expired", "InvalidCertificate"],
        );
    }

    #[test]
    fn mtls_not_yet_valid_client_cert_rejected() {
        // Threat A5: a not-yet-valid client cert must be rejected.
        ensure_crypto_provider();
        let (ca, ca_key) = gen_ca();
        let (srv_cfg, _c, _k, _a) = mtls_server(&ca, &ca_key, &[]);
        let now = time::OffsetDateTime::now_utc();
        let (future, key) = gen_client_leaf_validity(
            &ca,
            &ca_key,
            now + time::Duration::days(1),
            now + time::Duration::days(30),
        );
        let cli = client_config(ca.der(), Some((future.der().clone(), key_der(&key))));
        expect_rejected(
            do_handshake(srv_cfg, cli),
            &["NotValidYet", "InvalidCertificate"],
        );
    }

    /// End-to-end on the auto-generated + issued materials: a daemon that mints
    /// its own CA + server cert (ensure_server_materials) and a client cert issued
    /// from that CA (issue_client_cert) complete a mutually-authenticated TLS 1.3
    /// handshake. This proves the secure-by-default path produces a working,
    /// chaining cert set with the right profiles.
    #[test]
    fn autogen_materials_and_issued_client_complete_mtls_handshake() {
        ensure_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let mats = ensure_server_materials(dir.path(), &[]).unwrap();

        let srv_cfg = build_mtls_server_config(
            mats.server_cert_path.to_str().unwrap(),
            mats.server_key_path.to_str().unwrap(),
            mats.ca_cert_path.to_str().unwrap(),
            &[],
            "",
        )
        .unwrap();

        let ca_cert_pem = std::fs::read_to_string(&mats.ca_cert_path).unwrap();
        let ca_key_pem = std::fs::read_to_string(&mats.ca_key_path).unwrap();
        let client = issue_client_cert(&ca_cert_pem, &ca_key_pem, "device-abc").unwrap();

        let client_cert = load_certs_from_pem(&client.cert_pem);
        let client_key = load_key_from_pem(&client.key_pem);
        let ca_der = load_certs_from_pem(&ca_cert_pem)[0].clone();

        let cli = client_config(&ca_der, Some((client_cert[0].clone(), client_key)));
        let version = do_handshake(srv_cfg, cli).expect("auto-gen + issued client must handshake");
        assert_eq!(version, rustls::ProtocolVersion::TLSv1_3);
    }

    fn load_certs_from_pem(pem: &str) -> Vec<CertificateDer<'static>> {
        rustls_pemfile::certs(&mut pem.as_bytes())
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    fn load_key_from_pem(pem: &str) -> PrivateKeyDer<'static> {
        rustls_pemfile::private_key(&mut pem.as_bytes())
            .unwrap()
            .expect("client key present")
    }
}
